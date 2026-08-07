//! Self-update awareness: does a newer `origin/master` exist than the build
//! that is currently running, and can this machine safely close the gap?
//!
//! Three different kinds of "out of date" exist and the UI must tell them
//! apart, because each has a different fix:
//!
//! - **Repo behind**: `origin/master` carries commits this checkout lacks.
//!   Fix: pull.
//! - **Binary behind**: the checkout is current but the running executable was
//!   compiled earlier. Fix: rebuild. This one is invisible from `git status`
//!   alone, and it is the state a machine lands in after "I pulled but never
//!   rebuilt" (a whole fleet drifted onto three different version numbers that
//!   way).
//! - **Restart pending**: a rebuild finished, so a newer executable is sitting
//!   on disk, but this process is still the old one. Fix: restart. Reporting
//!   "up to date" here would be a lie the user could see through in the footer.
//!
//! The comparison is possible because `THREADKNOT_VERSION` is baked in at compile
//! time from the commit count of `origin/master` (see `build.rs`), so the
//! embedded number is a snapshot of "master as of my build" while a live
//! `rev-list --count` gives master as of now. The delta between them is exactly
//! the set of commits the running app is missing, regardless of where HEAD sits.
//!
//! Every request kind here starts with `git.` on purpose: `server::is_routable`
//! routes the whole git family by `machineId`, so the same call answers for a
//! peer machine and the settings tab can show the whole fleet for free. That
//! routing is also why every safety rule below is enforced *here*, on the
//! machine that owns the checkout, and never on the machine that clicked: a
//! controller must not be able to talk a peer into a pull it considers unsafe.

use crate::agents::Hub;
use crate::protocol::{now_iso, ThreadStatus};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::sync::Notify;

/// Background re-check cadence. A fetch is cheap but not free, and master does
/// not move minute to minute.
const POLL_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// Collapse a burst of manual "check now" clicks.
const KICK_DEBOUNCE: Duration = Duration::from_secs(2);
/// Never list more than this many pending commits (the UI scrolls, but the
/// payload should stay bounded).
const COMMIT_CAP: u64 = 50;
/// How long a build may run before we call it stuck. A cold release compile is
/// minutes, not seconds, so this is generous on purpose.
const BUILD_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// Tail of the build log kept in the error message. Enough to see the compiler
/// error, small enough not to flood a websocket frame.
const LOG_TAIL_BYTES: usize = 4_000;

/// When this process started. Anything on disk newer than this was written
/// after we launched, which is exactly what "restart to load it" means.
static STARTED_AT: OnceLock<SystemTime> = OnceLock::new();

fn started_at() -> SystemTime {
    *STARTED_AT.get_or_init(SystemTime::now)
}

/// Shared last-known snapshot, mirroring `usage::UsageState`.
#[derive(Default)]
pub struct UpdateState {
    data: Mutex<Option<UpdateStatus>>,
    /// The one in-flight pull/rebuild/restart, if any. Doubles as the operation
    /// lock: a new one is refused while this holds an unfinished entry.
    op: Mutex<Option<Operation>>,
    pub kick: Notify,
    force: AtomicBool,
}

impl UpdateState {
    pub fn snapshot(&self) -> Option<UpdateStatus> {
        self.data.lock().unwrap().clone()
    }

    fn store(&self, next: UpdateStatus) {
        *self.data.lock().unwrap() = Some(next);
    }

    pub fn operation(&self) -> Option<Operation> {
        self.op.lock().unwrap().clone()
    }

    /// Claim the operation lock. The backend is authoritative here on purpose:
    /// a disabled button in one browser tab does not stop a second tab, a
    /// phone, or a peer machine from firing the same action concurrently. Two
    /// `cargo build`s in one target directory is a corrupted build, not a
    /// slow one.
    fn begin(&self, kind: &str) -> Result<Operation> {
        let mut slot = self.op.lock().unwrap();
        if let Some(running) = slot.as_ref().filter(|o| o.finished_at.is_none()) {
            anyhow::bail!(
                "{} is already running on this machine (started {}). Wait for it to finish.",
                running.kind,
                running.started_at
            );
        }
        let op = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            stage: "preparing".into(),
            started_at: now_iso(),
            finished_at: None,
            ok: None,
            error: None,
            log_path: None,
        };
        *slot = Some(op.clone());
        Ok(op)
    }

    /// Advance the stage of the in-flight operation. Ignored if it already
    /// finished, so a late-arriving stage cannot resurrect a closed operation.
    fn stage(&self, id: &str, stage: &str) {
        let mut slot = self.op.lock().unwrap();
        if let Some(op) = slot.as_mut().filter(|o| o.id == id && o.finished_at.is_none()) {
            op.stage = stage.to_string();
        }
    }

    fn set_log(&self, id: &str, path: &Path) {
        let mut slot = self.op.lock().unwrap();
        if let Some(op) = slot.as_mut().filter(|o| o.id == id) {
            op.log_path = Some(path.display().to_string());
        }
    }

    fn finish(&self, id: &str, ok: bool, stage: &str, error: Option<String>) {
        let mut slot = self.op.lock().unwrap();
        if let Some(op) = slot.as_mut().filter(|o| o.id == id) {
            op.stage = stage.to_string();
            op.finished_at = Some(now_iso());
            op.ok = Some(ok);
            op.error = error;
        }
    }

    /// Ask the poller to re-check now.
    pub fn kick(&self, force: bool) {
        if force {
            self.force.store(true, Ordering::Relaxed);
        }
        self.kick.notify_one();
    }
}

/// One pull / rebuild / restart, from claim to completion. Surfaced verbatim to
/// the UI so a long build shows a stage instead of a frozen button.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: String,
    /// "pull" | "rebuild" | "restart".
    pub kind: String,
    pub stage: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    /// None while running.
    pub ok: Option<bool>,
    pub error: Option<String>,
    /// Full build output stays on disk; only a tail ever reaches the UI.
    pub log_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub hash: String,
    pub date: String,
    pub subject: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// False when this build was made outside a git checkout, or the checkout
    /// has since moved or been deleted. Everything else is then meaningless and
    /// the UI hides the section.
    pub repo_available: bool,
    pub repo_path: String,
    /// How `repo_path` was found: "env" | "configured" | "embedded" |
    /// "executable" | "none". Shown in the UI so a machine that resolved the
    /// wrong checkout is diagnosable without reading logs.
    pub repo_source: String,
    /// Version string the running process was compiled with, e.g. "0.1.67" or
    /// "0.1.56-dev4".
    pub running_version: String,
    /// "0.1.<live count of origin/master>", or None when it cannot be read.
    pub master_version: Option<String>,
    /// True when master's commit count exceeds the running build's. This is the
    /// single signal that drives the gear pulse.
    pub update_available: bool,
    /// Number of commits master has that the running build does not.
    pub behind_by: u64,
    /// Checkout is already at master but the binary predates it: rebuild only,
    /// no pull needed.
    pub rebuild_pending: bool,
    /// A newer executable is already on disk; only a restart is left. Takes
    /// precedence in the UI over `rebuild_pending`, because rebuilding again
    /// would just reproduce the same binary.
    pub restart_pending: bool,
    /// Whether this platform can restart itself in place.
    pub restart_supported: bool,
    /// Whether this platform can rebuild itself in place.
    pub rebuild_supported: bool,
    /// Executable a restart would launch.
    pub binary_path: String,
    /// Threads mid-turn or waiting on an approval. Restarting through these
    /// interrupts real work, so the UI asks first.
    pub active_work: u64,
    pub branch: String,
    /// Commits on master but not on HEAD, and vice versa. Drives the "keep your
    /// own branch" safety rules.
    pub head_behind: u64,
    pub head_ahead: u64,
    pub dirty: bool,
    /// Can master be reached by fast-forward alone? False means we refuse to
    /// touch it automatically.
    pub can_fast_forward: bool,
    /// What landed on master since the running build, newest first.
    pub commits: Vec<Commit>,
    pub checked_at: String,
    pub error: Option<String>,
    /// Most recent pull/rebuild/restart, running or finished.
    pub operation: Option<Operation>,
}

impl UpdateStatus {
    /// No usable answer. `repo_found` separates "there is no checkout here"
    /// (the user must point us at one) from "the checkout is fine but git could
    /// not answer" (fetch failed, no origin/master yet). Conflating the two made
    /// the UI demand a new source folder when the folder was never the problem.
    fn unavailable(hub: &Arc<Hub>, reason: impl Into<String>, repo_found: bool) -> Self {
        let (path, source) = match resolve_repo(hub) {
            Some((p, s)) => (p.display().to_string(), s),
            None => (String::new(), "none"),
        };
        Self {
            repo_available: repo_found,
            repo_path: path,
            repo_source: source.to_string(),
            running_version: env!("THREADKNOT_VERSION").to_string(),
            master_version: None,
            update_available: false,
            behind_by: 0,
            rebuild_pending: false,
            restart_pending: false,
            restart_supported: cfg!(unix),
            rebuild_supported: cfg!(unix),
            binary_path: restart_target(None).display().to_string(),
            active_work: active_work(hub),
            branch: String::new(),
            head_behind: 0,
            head_ahead: 0,
            dirty: false,
            can_fast_forward: false,
            commits: Vec::new(),
            checked_at: now_iso(),
            error: Some(reason.into()),
            operation: hub.updates.operation(),
        }
    }
}

// ---- repository discovery -------------------------------------------------

/// Where the machine-local repo-path override lives. Machine-local on purpose:
/// this path is a property of one physical machine's disk, so it must never
/// replicate across the mesh the way workspaces do.
fn override_file(hub: &Arc<Hub>) -> PathBuf {
    hub.store.dir().join("update.json")
}

fn configured_path(hub: &Arc<Hub>) -> Option<String> {
    let raw = std::fs::read_to_string(override_file(hub)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let p = v.get("repoPath")?.as_str()?.trim().to_string();
    (!p.is_empty()).then_some(p)
}

fn set_configured_path(hub: &Arc<Hub>, path: Option<&str>) -> Result<()> {
    let file = override_file(hub);
    match path {
        Some(p) => {
            let dir = PathBuf::from(p);
            anyhow::ensure!(
                dir.join(".git").exists(),
                "{p} is not a git checkout (no .git directory there)."
            );
            std::fs::write(&file, serde_json::to_string_pretty(&json!({ "repoPath": p }))?)
                .with_context(|| format!("could not save {}", file.display()))?;
        }
        // Clearing falls back to the next source down rather than disabling
        // updates outright.
        None => {
            let _ = std::fs::remove_file(&file);
        }
    }
    Ok(())
}

/// A checkout beside the running executable: `<repo>/src-tauri/target/<profile>/threadknot`.
/// This is what makes a binary portable between machines and platforms. The
/// baked-in path is a Linux path on a Linux build, so a copy running on Windows
/// must be able to find its own checkout without it.
fn exe_relative_repo() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // release/ -> target/ -> src-tauri/ -> repo root.
    let repo = exe.parent()?.parent()?.parent()?.parent()?;
    repo.join(".git").exists().then(|| repo.to_path_buf())
}

/// Resolve the source checkout, most specific source first. Every candidate is
/// validated before it is accepted, which is what stops a Linux binary copied
/// to Windows from insisting on `/home/...`: that path simply fails the `.git`
/// check and the search falls through to the executable-relative one.
fn resolve_repo(hub: &Arc<Hub>) -> Option<(PathBuf, &'static str)> {
    let valid = |p: PathBuf| p.join(".git").exists().then_some(p);

    if let Ok(dir) = std::env::var("THREADKNOT_REPO_DIR") {
        if !dir.is_empty() {
            if let Some(p) = valid(PathBuf::from(&dir)) {
                return Some((p, "env"));
            }
        }
    }
    if let Some(dir) = configured_path(hub) {
        if let Some(p) = valid(PathBuf::from(&dir)) {
            return Some((p, "configured"));
        }
    }
    let embedded = env!("THREADKNOT_REPO_DIR");
    if !embedded.is_empty() {
        if let Some(p) = valid(PathBuf::from(embedded)) {
            return Some((p, "embedded"));
        }
    }
    exe_relative_repo().map(|p| (p, "executable"))
}

/// Every path we looked at, for an error message that can actually be acted on.
fn candidates(hub: &Arc<Hub>) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(d) = std::env::var("THREADKNOT_REPO_DIR") {
        if !d.is_empty() {
            out.push(format!("THREADKNOT_REPO_DIR={d}"));
        }
    }
    if let Some(d) = configured_path(hub) {
        out.push(format!("configured: {d}"));
    }
    let embedded = env!("THREADKNOT_REPO_DIR");
    if !embedded.is_empty() {
        out.push(format!("build-time: {embedded}"));
    }
    if let Ok(exe) = std::env::current_exe() {
        out.push(format!("next to: {}", exe.display()));
    }
    out
}

// ---- runtime facts --------------------------------------------------------

/// `current_exe`, with Linux's "(deleted)" marker removed.
///
/// `/proc/self/exe` reports a replaced executable as "<path> (deleted)", and a
/// rebuild replaces the binary by rename. That makes this the *normal* state
/// immediately after this feature's own rebuild step, not an exotic one. Left
/// as-is the suffix becomes part of the file name, so the freshly built binary
/// is never found, `restart_pending` stays false and the restart button never
/// appears after a successful rebuild. The path without the marker is also the
/// right thing to launch: it now holds the new build.
fn strip_deleted_marker(current: PathBuf) -> PathBuf {
    match current.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(clean) => PathBuf::from(clean),
        None => current,
    }
}

fn current_exe_path() -> PathBuf {
    strip_deleted_marker(std::env::current_exe().unwrap_or_else(|_| PathBuf::from("threadknot")))
}

/// Executable a restart should launch: the freshly built release binary when
/// the checkout has one, otherwise whatever is running now. Checking the build
/// output rather than only `current_exe` is what lets a dev running the debug
/// binary still be told a new release binary is waiting.
fn restart_target(repo: Option<&Path>) -> PathBuf {
    let current = current_exe_path();
    let name = current.file_name().unwrap_or_default().to_owned();
    if let Some(repo) = repo {
        let built = repo.join("src-tauri").join("target").join("release").join(&name);
        if built.is_file() {
            return built;
        }
    }
    current
}

/// True when `path` was written after `reference`. Split out from
/// `binary_is_newer` so it can be tested without depending on when the test
/// binary happened to start.
fn newer_than(path: &Path, reference: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|m| m > reference)
        .unwrap_or(false)
}

/// True when the executable a restart would launch was written after this
/// process started, i.e. restarting actually loads something new.
fn binary_is_newer(target: &Path) -> bool {
    newer_than(target, started_at())
}

/// Threads mid-turn or blocked on an approval, across every project.
fn active_work(hub: &Arc<Hub>) -> u64 {
    hub.store
        .list_projects()
        .iter()
        .flat_map(|p| hub.store.list_threads(&p.id))
        .filter(|t| matches!(t.status, ThreadStatus::Running | ThreadStatus::WaitingApproval))
        .count() as u64
}

/// Trailing commit count of a version string: "0.1.67" -> 67,
/// "0.1.56-dev4" -> 56 (the dev suffix describes local commits, not master).
fn version_count(v: &str) -> Option<u64> {
    let last = v.rsplit('.').next()?;
    let digits: String = last.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Parse `%h\x1f%ad\x1f%s` records, one per line.
fn parse_commits(raw: &str) -> Vec<Commit> {
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut f = line.split('\u{1f}');
            Commit {
                hash: f.next().unwrap_or("").trim().to_string(),
                date: f.next().unwrap_or("").trim().to_string(),
                subject: f.next().unwrap_or("").trim().to_string(),
            }
        })
        .collect()
}

/// Last bytes of a build log, for an error the user can act on without opening
/// a file. Truncated at a char boundary so the string stays valid UTF-8.
fn log_tail(path: &Path) -> String {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    if raw.len() <= LOG_TAIL_BYTES {
        return raw.trim().to_string();
    }
    let mut cut = raw.len() - LOG_TAIL_BYTES;
    while cut < raw.len() && !raw.is_char_boundary(cut) {
        cut += 1;
    }
    raw[cut..].trim().to_string()
}

// ---- status ---------------------------------------------------------------

/// Read the repo's update situation. `fetch` performs a network fetch first;
/// callers that only want to render cached refs pass false.
pub async fn compute(hub: &Arc<Hub>, fetch: bool) -> UpdateStatus {
    let Some((repo, source)) = resolve_repo(hub) else {
        let tried = candidates(hub);
        let detail = if tried.is_empty() {
            "no candidate paths were available".to_string()
        } else {
            format!("looked in — {}", tried.join("; "))
        };
        return UpdateStatus::unavailable(
            hub,
            format!(
                "No Threadknot git checkout found on this machine, so it cannot check for \
                 updates ({detail}). Set the source folder below to fix this."
            ),
            false,
        );
    };
    match compute_in(hub, &repo, source, fetch).await {
        Ok(st) => st,
        Err(e) => UpdateStatus::unavailable(hub, format!("{e:#}"), true),
    }
}

async fn compute_in(
    hub: &Arc<Hub>,
    repo: &Path,
    source: &str,
    fetch: bool,
) -> Result<UpdateStatus> {
    // A failed fetch is not fatal: cached refs still say something useful, and
    // the error rides along so the UI can say "offline, showing last known".
    let mut fetch_error: Option<String> = None;
    if fetch {
        if let Err(e) = crate::git::run_git(repo, &["fetch", "--quiet", "origin", "master"]).await {
            fetch_error = Some(format!("{e:#}"));
        }
    }

    let master_ref = "origin/master";
    anyhow::ensure!(
        crate::git::run_git(repo, &["rev-parse", "--verify", "--quiet", master_ref])
            .await
            .is_ok(),
        "no origin/master ref in {}. has it ever been fetched?",
        repo.display()
    );

    let master_count: u64 = crate::git::run_git(repo, &["rev-list", "--count", master_ref])
        .await
        .context("counting master commits")?
        .trim()
        .parse()
        .context("master commit count was not a number")?;

    let running_version = env!("THREADKNOT_VERSION").to_string();
    let running_count = version_count(&running_version).unwrap_or(0);

    let branch = crate::git::run_git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    let dirty = !crate::git::run_git(repo, &["status", "--porcelain"])
        .await
        .unwrap_or_default()
        .trim()
        .is_empty();

    // left = on master only (we are behind), right = on HEAD only (ahead).
    let (head_behind, head_ahead) = match crate::git::run_git(
        repo,
        &["rev-list", "--left-right", "--count", "origin/master...HEAD"],
    )
    .await
    {
        Ok(raw) => {
            let mut it = raw.split_whitespace();
            (
                it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
                it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            )
        }
        Err(_) => (0, 0),
    };

    let behind_by = master_count.saturating_sub(running_count);
    // Two axes can lag, and the list has to cover whichever is further behind:
    // `behind_by` is what the running BINARY lacks (stays right when the checkout
    // is already current and only a rebuild is owed), `head_behind` is what a
    // pull would actually bring down. Showing only the smaller of the two
    // understated the change set the user is about to accept.
    let show = behind_by.max(head_behind);
    let commits = if show == 0 {
        Vec::new()
    } else {
        let n = show.min(COMMIT_CAP).to_string();
        match crate::git::run_git(
            repo,
            &[
                "log",
                "-n",
                &n,
                "--date=short",
                "--pretty=%h\u{1f}%ad\u{1f}%s",
                master_ref,
            ],
        )
        .await
        {
            Ok(raw) => parse_commits(&raw),
            Err(_) => Vec::new(),
        }
    };

    let target = restart_target(Some(repo));
    let restart_pending = binary_is_newer(&target);

    Ok(UpdateStatus {
        repo_available: true,
        repo_path: repo.display().to_string(),
        repo_source: source.to_string(),
        running_version,
        master_version: Some(format!("0.1.{master_count}")),
        // Either axis counts. A dev whose personal branch has more commits than
        // master makes `behind_by` saturate to 0, and without `head_behind` the
        // pull that is genuinely waiting would be invisible.
        update_available: behind_by > 0 || head_behind > 0,
        behind_by,
        // Nothing to pull, yet the binary is old: a rebuild is all that is left.
        // Once a rebuild has actually produced a newer binary, the remaining
        // step is a restart, so this stands down rather than inviting a second
        // identical build.
        rebuild_pending: behind_by > 0 && head_behind == 0 && !restart_pending,
        restart_pending,
        restart_supported: cfg!(unix),
        rebuild_supported: cfg!(unix),
        binary_path: target.display().to_string(),
        active_work: active_work(hub),
        branch,
        head_behind,
        head_ahead,
        dirty,
        // Only safe when we are strictly behind with no local divergence.
        can_fast_forward: head_behind > 0 && head_ahead == 0 && !dirty,
        commits,
        checked_at: now_iso(),
        error: fetch_error,
        operation: hub.updates.operation(),
    })
}

/// Recompute and broadcast. Used after every operation so a card can never keep
/// showing the state the user just changed.
async fn refresh(hub: &Arc<Hub>) {
    let next = compute(hub, false).await;
    hub.updates.store(next);
    hub.broadcast_state("updates", None);
}

/// Poll for a newer master and broadcast when the answer changes.
pub fn spawn_poller(hub: Arc<Hub>) {
    started_at();
    tokio::spawn(async move {
        loop {
            // The first pass and every manual kick fetch; otherwise we would
            // only ever re-read stale cached refs.
            let force = hub.updates.force.swap(false, Ordering::Relaxed);
            let next = compute(&hub, true).await;
            let changed = hub.updates.snapshot().as_ref() != Some(&next);
            hub.updates.store(next);
            if changed || force {
                hub.broadcast_state("updates", None);
            }

            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = hub.updates.kick.notified() => {
                    tokio::time::sleep(KICK_DEBOUNCE).await;
                }
            }
        }
    });
}

// ---- pull -----------------------------------------------------------------

/// Fast-forward the checkout onto `origin/master`. Deliberately refuses
/// anything that would rewrite or merge local work: a machine carrying
/// unpushed commits is exactly where an automated pull does damage.
async fn fast_forward(hub: &Arc<Hub>, repo: &Path, source: &str) -> Result<Value> {
    // No inline fetch: the poller refreshed the refs, and a slow fetch here
    // would blow past the client's 30s request timeout mid-merge.
    let st = compute_in(hub, repo, source, false).await?;
    anyhow::ensure!(
        !st.dirty,
        "Pull blocked: this checkout has uncommitted changes. Commit or stash them first."
    );
    anyhow::ensure!(
        st.head_ahead == 0,
        "Pull blocked: this machine is {} commit(s) ahead of master on branch '{}'. \
         Push or land that work first — Threadknot will not rebase, merge, reset or discard it.",
        st.head_ahead,
        st.branch
    );
    anyhow::ensure!(
        st.head_behind > 0,
        "Already up to date with master; only a rebuild is needed."
    );
    // `--ff-only` is the whole safety story: git itself refuses if the history
    // diverged, so there is no window where we could merge or rebase by accident.
    crate::git::run_git(repo, &["merge", "--ff-only", "origin/master"])
        .await
        .context("fast-forward to origin/master failed")?;
    Ok(json!({ "ok": true }))
}

// ---- rebuild --------------------------------------------------------------

/// Run one build stage, streaming both streams into the log. Fixed argv, no
/// shell: nothing the frontend sends reaches a command line.
async fn build_stage(repo: &Path, log: &Path, program: &str, args: &[&str]) -> Result<()> {
    use std::process::Stdio;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("cannot write build log {}", log.display()))?;
    let errs = file.try_clone()?;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(repo)
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(errs));
    // Windows: keep the build's console window from popping up (see git.rs).
    crate::agents::no_console(&mut cmd);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not start {program} — is it installed?"))?;
    let status = match tokio::time::timeout(BUILD_TIMEOUT, child.wait()).await {
        Ok(s) => s.context("build process failed")?,
        Err(_) => {
            let _ = child.start_kill();
            anyhow::bail!("{program} did not finish within 45 minutes and was stopped");
        }
    };
    anyhow::ensure!(
        status.success(),
        "{program} {} exited with {}",
        args.first().copied().unwrap_or(""),
        status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into())
    );
    Ok(())
}

/// Rebuild the release binary in place, in this process, with the exit code
/// and log captured. Deliberately NOT a detached fire-and-forget: the previous
/// shape reported success the instant the shell started, so a failing compile
/// looked identical to a good one.
///
/// Overwriting the running executable is safe on Unix because cargo renames the
/// new file into place and this process keeps the old inode until it exits.
/// That is precisely why the restart is a separate, explicit step.
async fn run_rebuild(hub: Arc<Hub>, repo: PathBuf, op_id: String) {
    let log = repo.join("src-tauri").join("target").join("threadknot-rebuild.log");
    let _ = std::fs::create_dir_all(log.parent().unwrap_or(&repo));
    let _ = std::fs::write(&log, format!("==== rebuild {} ====\n", now_iso()));
    hub.updates.set_log(&op_id, &log);

    let stages: [(&str, &str, Vec<&str>); 2] = [
        // `npm run build` is `tsc --noEmit && vite build`, so a type error fails
        // here in seconds instead of after a multi-minute compile.
        ("building frontend", "npm", vec!["run", "build"]),
        (
            "compiling backend",
            "cargo",
            vec!["build", "--release", "--bins", "--manifest-path", "src-tauri/Cargo.toml"],
        ),
    ];

    for (stage, program, args) in stages {
        hub.updates.stage(&op_id, stage);
        refresh(&hub).await;
        if let Err(e) = build_stage(&repo, &log, program, &args).await {
            let tail = log_tail(&log);
            hub.updates.finish(
                &op_id,
                false,
                "rebuild failed",
                Some(format!("{e:#}\n\n{tail}")),
            );
            refresh(&hub).await;
            return;
        }
    }

    hub.updates.stage(&op_id, "verifying binary");
    let target = restart_target(Some(&repo));
    if !target.is_file() {
        hub.updates.finish(
            &op_id,
            false,
            "rebuild failed",
            Some(format!("the build finished but {} does not exist", target.display())),
        );
        refresh(&hub).await;
        return;
    }
    // The build can "succeed" without producing anything new (nothing to do,
    // or output written somewhere else). Saying "restart to load it" then would
    // send the user in a circle.
    if !binary_is_newer(&target) {
        hub.updates.finish(
            &op_id,
            true,
            "rebuild complete, binary unchanged",
            Some(format!(
                "The build succeeded but {} is not newer than the running process. \
                 Nothing to restart into.",
                target.display()
            )),
        );
        refresh(&hub).await;
        return;
    }
    hub.updates.finish(&op_id, true, "rebuild complete, restart required", None);
    refresh(&hub).await;
}

// ---- restart --------------------------------------------------------------

/// Replace the running app with the executable on disk.
///
/// Targets exactly this process by pid. Nothing here matches on a process name,
/// so no unrelated Threadknot, Claude, Codex, terminal or editor can be caught by
/// it. The child is session-detached so killing us cannot take it down, and it
/// inherits our environment, which is what keeps the relaunched window attached
/// to the same desktop session.
fn spawn_restart(target: &Path) -> Result<()> {
    anyhow::ensure!(
        cfg!(unix),
        "Automatic restart is not wired up on this platform yet. Close Threadknot and \
         open it again to load the new build."
    );
    anyhow::ensure!(
        target.is_file(),
        "nothing to restart into: {} does not exist",
        target.display()
    );
    let script = format!(
        // TERM first and only escalate if it is ignored, so the app gets its
        // normal shutdown path and its pending writes land.
        "kill -TERM {pid} 2>/dev/null || true\n\
         for _ in $(seq 1 40); do kill -0 {pid} 2>/dev/null || break; sleep 0.25; done\n\
         if kill -0 {pid} 2>/dev/null; then kill -KILL {pid} 2>/dev/null || true; sleep 1; fi\n\
         exec {exe}\n",
        pid = std::process::id(),
        exe = shell_quote(&target.display().to_string()),
    );
    let (program, lead) = if which("setsid") {
        ("setsid", vec!["sh", "-c"])
    } else {
        ("sh", vec!["-c"])
    };
    let mut cmd = std::process::Command::new(program);
    for a in lead {
        cmd.arg(a);
    }
    cmd.arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().context("could not start the restart helper")?;
    Ok(())
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Single-quote for `sh -c`, escaping embedded quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---- dispatch -------------------------------------------------------------

/// Update kinds that change something: the source checkout, the binary on
/// disk, or the life of the process. Callers must hold the master token.
/// Status reads are deliberately absent so a paired phone can still see the
/// fleet's versions without being able to act on them.
pub fn is_privileged(kind: &str) -> bool {
    matches!(
        kind,
        "git.selfUpdatePull"
            | "git.selfUpdateRebuild"
            | "git.selfUpdateRestart"
            | "git.selfUpdateSetRepoPath"
    )
}

/// Resolve the checkout or explain why there is none.
fn require_repo(hub: &Arc<Hub>) -> Result<(PathBuf, &'static str)> {
    resolve_repo(hub).context(
        "No Threadknot git checkout found on this machine. Set the source folder in \
         Settings → Updates first.",
    )
}

/// Dispatch `git.selfUpdate*`. Routed by machineId like the rest of the git
/// family, so the settings tab can ask any peer the same questions.
///
/// Every rule below runs on the machine that owns the checkout. A controller
/// machine cannot pre-approve a pull, skip the dirty check, or bypass the
/// operation lock on someone else's behalf.
pub async fn handle(hub: &Arc<Hub>, kind: &str, payload: &Value) -> Result<Value> {
    match kind {
        // Cheap read for rendering. `fetch: true` forces a network refresh.
        "git.selfUpdateStatus" => {
            let fetch = payload.get("fetch").and_then(Value::as_bool).unwrap_or(false);
            let status = match (fetch, hub.updates.snapshot()) {
                (false, Some(cached)) => UpdateStatus {
                    // The cached body is fine, but these two move without a
                    // re-poll and a stale operation is exactly what makes a
                    // progress display useless.
                    operation: hub.updates.operation(),
                    active_work: active_work(hub),
                    ..cached
                },
                _ => {
                    let fresh = compute(hub, fetch).await;
                    hub.updates.store(fresh.clone());
                    fresh
                }
            };
            Ok(serde_json::to_value(status)?)
        }
        // Manual "check now". Kicks the poller rather than fetching inline: a
        // slow fetch gets 120s server-side but the client gives up at 30s, so
        // the fresh answer arrives via the `updates` broadcast instead.
        "git.selfUpdateCheck" => {
            hub.updates.kick(true);
            Ok(json!({}))
        }
        "git.selfUpdatePull" => {
            let (repo, source) = require_repo(hub)?;
            let op = hub.updates.begin("pull")?;
            hub.updates.stage(&op.id, "pulling");
            let out = fast_forward(hub, &repo, source).await;
            match &out {
                Ok(_) => hub.updates.finish(&op.id, true, "repository current", None),
                Err(e) => hub.updates.finish(&op.id, false, "pull failed", Some(format!("{e:#}"))),
            }
            hub.updates.kick(true);
            out
        }
        "git.selfUpdateRebuild" => {
            let (repo, _) = require_repo(hub)?;
            anyhow::ensure!(
                cfg!(unix),
                "Automatic rebuild is only wired up for Linux and macOS so far. \
                 Run `npx tauri build --no-bundle` in {} and reopen Threadknot.",
                repo.display()
            );
            let op = hub.updates.begin("rebuild")?;
            // Detached task, not a detached shell: the build outlives the
            // request (which would time out at 30s) but stays observable, so
            // its exit code and log are real rather than assumed.
            tokio::spawn(run_rebuild(hub.clone(), repo, op.id.clone()));
            Ok(json!({ "ok": true, "operationId": op.id }))
        }
        "git.selfUpdateRestart" => {
            let repo = resolve_repo(hub).map(|(p, _)| p);
            let target = restart_target(repo.as_deref());
            let force = payload.get("force").and_then(Value::as_bool).unwrap_or(false);
            let busy = active_work(hub);
            anyhow::ensure!(
                force || busy == 0,
                "{busy} thread(s) are mid-turn or waiting on approval on this machine. \
                 Restart when they finish, or confirm to restart anyway."
            );
            let op = hub.updates.begin("restart")?;
            hub.updates.stage(&op.id, "restarting");
            match spawn_restart(&target) {
                Ok(()) => {
                    // Nothing after this is guaranteed to run: the helper is
                    // already counting down to kill us.
                    hub.updates.finish(&op.id, true, "restarting", None);
                    Ok(json!({ "ok": true, "restarting": true }))
                }
                Err(e) => {
                    hub.updates.finish(&op.id, false, "restart failed", Some(format!("{e:#}")));
                    refresh(hub).await;
                    Err(e)
                }
            }
        }
        // Machine-local repo path override, for a packaged build or a checkout
        // that moved. Never replicated to peers.
        "git.selfUpdateSetRepoPath" => {
            let path = payload.get("path").and_then(Value::as_str).map(str::trim);
            set_configured_path(hub, path.filter(|p| !p.is_empty()))?;
            hub.updates.kick(true);
            Ok(json!({ "ok": true }))
        }
        other => anyhow::bail!("unknown update request: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_count_reads_clean_and_dev_builds() {
        assert_eq!(version_count("0.1.67"), Some(67));
        // A dev suffix counts local commits, not master, so it must not shift
        // the number the fleet comparison is based on.
        assert_eq!(version_count("0.1.56-dev4"), Some(56));
        assert_eq!(version_count("0.1.56-dev"), Some(56));
        assert_eq!(version_count("garbage"), None);
    }

    #[test]
    fn parse_commits_splits_unit_separated_records() {
        let raw = "abc123\u{1f}2026-07-25\u{1f}fix: a thing\ndef456\u{1f}2026-07-24\u{1f}feat: another";
        let got = parse_commits(raw);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].hash, "abc123");
        assert_eq!(got[0].subject, "fix: a thing");
        assert_eq!(got[1].date, "2026-07-24");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("/plain/path"), "'/plain/path'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    /// Scratch directory that cleans up after itself, so these tests need no
    /// extra dependency just to hold two files.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!("threadknot-update-test-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn log_tail_keeps_the_end_and_stays_utf8() {
        let dir = Scratch::new("log");
        let p = dir.0.join("build.log");
        // Multi-byte characters straddling the cut must not panic or corrupt.
        let body = "é".repeat(LOG_TAIL_BYTES);
        std::fs::write(&p, format!("{body}TAIL-MARKER")).unwrap();
        let tail = log_tail(&p);
        assert!(tail.ends_with("TAIL-MARKER"));
        assert!(tail.len() <= LOG_TAIL_BYTES + 16);

        std::fs::write(&p, "short").unwrap();
        assert_eq!(log_tail(&p), "short");
    }

    /// The operation lock is the only thing standing between an impatient
    /// double-click and two cargo builds sharing one target directory.
    #[test]
    fn operation_lock_refuses_a_second_operation_until_the_first_finishes() {
        let st = UpdateState::default();
        let first = st.begin("rebuild").unwrap();
        let blocked = st.begin("rebuild").unwrap_err().to_string();
        assert!(blocked.contains("already running"), "got: {blocked}");
        // A different kind must not slip past the same lock.
        assert!(st.begin("pull").is_err());

        st.stage(&first.id, "compiling backend");
        assert_eq!(st.operation().unwrap().stage, "compiling backend");

        st.finish(&first.id, true, "rebuild complete", None);
        let done = st.operation().unwrap();
        assert_eq!(done.ok, Some(true));
        assert!(done.finished_at.is_some());
        // Lock released once the first one closed.
        assert!(st.begin("restart").is_ok());
    }

    #[test]
    fn stage_updates_are_ignored_once_an_operation_has_finished() {
        let st = UpdateState::default();
        let op = st.begin("pull").unwrap();
        st.finish(&op.id, false, "pull failed", Some("boom".into()));
        st.stage(&op.id, "pulling");
        let after = st.operation().unwrap();
        assert_eq!(after.stage, "pull failed");
        assert_eq!(after.ok, Some(false));
    }

    /// A binary that predates this process is not something to restart into.
    #[test]
    fn newer_than_distinguishes_pre_and_post_startup_writes() {
        let dir = Scratch::new("mtime");
        let f = dir.0.join("threadknot");
        std::fs::write(&f, "x").unwrap();

        // Written before this reference: restarting would load the same thing.
        assert!(!newer_than(&f, SystemTime::now() + Duration::from_secs(60)));
        // Written after this reference: a restart genuinely picks up new bytes.
        assert!(newer_than(&f, SystemTime::now() - Duration::from_secs(60)));
        // A build that produced nothing must never look like a pending restart.
        assert!(!newer_than(&dir.0.join("missing"), SystemTime::UNIX_EPOCH));
    }

    /// A restart must load the checkout's freshly built binary when there is
    /// one, and otherwise re-launch exactly what is running. Getting this wrong
    /// either strands the machine on the old build or launches a stranger.
    #[test]
    fn restart_target_prefers_the_checkouts_release_build() {
        let dir = Scratch::new("target");
        let current = std::env::current_exe().unwrap();
        let name = current.file_name().unwrap();

        // No build output yet: re-launch ourselves rather than guessing.
        assert_eq!(restart_target(Some(&dir.0)), current);
        assert_eq!(restart_target(None), current);

        let release = dir.0.join("src-tauri").join("target").join("release");
        std::fs::create_dir_all(&release).unwrap();
        // A directory of the right name is not an executable.
        std::fs::create_dir_all(release.join(name)).unwrap();
        assert_eq!(restart_target(Some(&dir.0)), current);

        std::fs::remove_dir(release.join(name)).unwrap();
        std::fs::write(release.join(name), "fresh build").unwrap();
        assert_eq!(restart_target(Some(&dir.0)), release.join(name));
    }

    /// Rebuilding replaces the running binary by rename, after which Linux
    /// reports it as "<path> (deleted)". That is the state this feature creates
    /// for itself, so the marker has to come off: while it is attached it
    /// becomes part of the file name, the new build is never found, and the
    /// restart step silently never offers itself.
    #[test]
    fn deleted_marker_is_stripped_from_the_running_exe_path() {
        assert_eq!(
            strip_deleted_marker(PathBuf::from("/opt/threadknot/threadknot (deleted)")),
            PathBuf::from("/opt/threadknot/threadknot")
        );
        // Only a real trailing marker counts; a path may legitimately contain
        // the word, and truncating those would point the restart somewhere else.
        assert_eq!(
            strip_deleted_marker(PathBuf::from("/opt/threadknot (deleted)/threadknot")),
            PathBuf::from("/opt/threadknot (deleted)/threadknot")
        );
        assert_eq!(
            strip_deleted_marker(PathBuf::from("/opt/threadknot/threadknot")),
            PathBuf::from("/opt/threadknot/threadknot")
        );
    }
}
