//! Self-update: is a newer Threadknot available than the build that is running,
//! and can this machine safely close the gap?
//!
//! There are two routes to a newer build, and a machine takes exactly one:
//!
//! - **The release channel** — the published builds on the project's GitHub
//!   Releases page. Threadknot downloads the artifact for this platform,
//!   replaces itself with it, and relaunches. This is what an installed copy
//!   does, and it is what "update" means in every other app.
//! - **The source channel** — the machine has a Threadknot git checkout, so the
//!   newer build is one it compiles itself: fast-forward to `origin/master`,
//!   rebuild, restart. This is the development route, and it is also the only
//!   route that can be a *commit* ahead rather than a release ahead.
//!
//! The route is derived, not asked: a machine with a checkout is a development
//! machine and updates from source; one without updates from releases. The
//! `git.selfUpdateSetChannel` request pins it either way, per machine, which is
//! how a development box tests what its users will actually experience.
//!
//! Both channels compare the same number. `THREADKNOT_VERSION` is
//! `0.1.<commit count of origin/master>` (see `build.rs`), and a release is
//! tagged `v0.1.<that same count>` at the commit it was cut from (see
//! `scripts/release.sh`), so "0.1.92" means the same build whether it was
//! compiled here or downloaded. Comparing them is comparing the trailing count.
//!
//! On the source channel, three different kinds of "out of date" exist and the
//! UI must tell them apart, because each has a different fix:
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
//! That comparison is possible because the embedded number is a snapshot of
//! "master as of my build" while a live `rev-list --count` gives master as of
//! now. The delta between them is exactly the set of commits the running app is
//! missing, regardless of where HEAD sits.
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
/// Whose GitHub Releases are the published builds. Overridable by
/// `THREADKNOT_RELEASE_REPO` so a fork updates from its own.
const DEFAULT_RELEASE_REPO: &str = "smith-network-solutions/threadknot";
/// Ceiling on how long the release check may take. Unauthenticated GitHub is
/// usually instant; the poller must not wedge when it is not.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(20);
/// Ceiling on downloading one release artifact. Installers are tens of
/// megabytes and some connections are not.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Refuse anything larger. An artifact this big is a wrong asset match or a
/// hostile response, not a Threadknot build.
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
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

/// Whether this machine can rebuild and restart itself in place.
///
/// The two platforms get there differently — Unix signals itself and re-execs
/// over the same path, Windows has to step out of the linker's way and relaunch
/// from a copy (see `free_build_target` and `spawn_restart_windows`) — but the
/// capability the UI offers is the same, so one flag answers for both.
const SELF_SERVICE: bool = cfg!(any(unix, windows));

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
    /// Last answer from the releases API, kept across the cheap non-fetching
    /// recomputes so a card is never blank between polls.
    release: Mutex<Option<ReleaseInfo>>,
    /// The one in-flight pull/rebuild/restart/install, if any. Doubles as the
    /// operation lock: a new one is refused while this holds an unfinished entry.
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

    fn release(&self) -> Option<ReleaseInfo> {
        self.release.lock().unwrap().clone()
    }

    fn store_release(&self, next: Option<ReleaseInfo>) {
        // A failed check keeps the last good answer rather than blanking the
        // card: "we could not reach GitHub just now" is not "there is no
        // release", and only one of those should change what the UI offers.
        if next.is_some() {
            *self.release.lock().unwrap() = next;
        }
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
    /// "pull" | "rebuild" | "restart" | "update" (the three chained together)
    /// | "install" (download a published release and relaunch into it).
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

/// The newest published release, and what this machine could do with it.
///
/// `asset_*` is already resolved to the one artifact this platform installs, so
/// the UI never has to know that a release carries a `.dmg`, three Linux
/// packages and an installer. When nothing here fits — a `.deb` install, which
/// only the package manager may replace — `blocked` says so in words the card
/// can show, and the download link stands in for the button.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInfo {
    /// Tag with the leading `v` removed: "0.1.92".
    pub version: String,
    pub tag: String,
    pub name: String,
    /// Release notes, truncated — this rides on every status broadcast.
    pub notes: String,
    pub published_at: String,
    /// The release page, for the manual route.
    pub url: String,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub asset_size: u64,
    /// Why this machine cannot install it in place, if it cannot.
    pub blocked: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// Which route this machine updates by: "release" (published builds) or
    /// "source" (its own checkout). Everything below about git is only the
    /// user's business on the source channel.
    pub channel: String,
    /// True when the channel was pinned by hand rather than derived from
    /// whether a checkout exists.
    pub channel_pinned: bool,
    /// Newest published release, once one has been read.
    pub release: Option<ReleaseInfo>,
    /// A published release is newer than the running build.
    pub release_available: bool,
    /// …and this machine can download and install it without help.
    pub release_install_supported: bool,
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
        // A release install lands a newer binary at the path we run from, and
        // the machine it lands on is exactly the one with no checkout — so this
        // branch has to notice a pending restart too, or an install whose final
        // relaunch did not take leaves no way back but the command line.
        let target = restart_target(None);
        Self {
            // Overwritten by `with_release` before this ever leaves the module.
            channel: Channel::Source.as_str().to_string(),
            channel_pinned: false,
            release: None,
            release_available: false,
            release_install_supported: false,
            repo_available: repo_found,
            repo_path: path,
            repo_source: source.to_string(),
            running_version: env!("THREADKNOT_VERSION").to_string(),
            master_version: None,
            update_available: false,
            behind_by: 0,
            rebuild_pending: false,
            restart_pending: binary_is_newer(&target),
            restart_supported: SELF_SERVICE,
            rebuild_supported: SELF_SERVICE,
            binary_path: target.display().to_string(),
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

/// Read the whole machine-local override file, so a write to one field cannot
/// silently drop the other.
fn overrides(hub: &Arc<Hub>) -> Value {
    std::fs::read_to_string(override_file(hub))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn write_override(hub: &Arc<Hub>, key: &str, value: Option<&str>) -> Result<()> {
    let file = override_file(hub);
    let mut doc = overrides(hub);
    let obj = doc.as_object_mut().expect("overrides() only returns objects");
    match value {
        Some(v) => {
            obj.insert(key.to_string(), json!(v));
        }
        None => {
            obj.remove(key);
        }
    }
    // An empty file is the same as no file, and leaving one behind means every
    // later read parses a document that says nothing.
    if obj.is_empty() {
        let _ = std::fs::remove_file(&file);
        return Ok(());
    }
    std::fs::write(&file, serde_json::to_string_pretty(&doc)?)
        .with_context(|| format!("could not save {}", file.display()))
}

fn set_configured_path(hub: &Arc<Hub>, path: Option<&str>) -> Result<()> {
    if let Some(p) = path {
        anyhow::ensure!(
            PathBuf::from(p).join(".git").exists(),
            "{p} is not a git checkout (no .git directory there)."
        );
    }
    // Clearing falls back to the next source down rather than disabling
    // updates outright.
    write_override(hub, "repoPath", path)
}

/// Which route a machine takes to a newer build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Channel {
    /// Published builds from the Releases page: download, install, relaunch.
    Release,
    /// This machine's own checkout: pull, rebuild, relaunch.
    Source,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Channel::Release => "release",
            Channel::Source => "source",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "release" => Some(Channel::Release),
            "source" => Some(Channel::Source),
            _ => None,
        }
    }
}

/// The channel this machine uses, and whether that was a decision or a default.
///
/// Derived from whether a checkout exists, because that is what the two
/// channels actually differ on: a machine that can compile master is a
/// development machine and wants the commit it just pushed, not the release cut
/// three weeks ago. Pinning exists so this box can sit on the release channel
/// and see exactly what a user sees.
fn resolve_channel(hub: &Arc<Hub>, repo_available: bool) -> (Channel, bool) {
    if let Some(c) = std::env::var("THREADKNOT_UPDATE_CHANNEL")
        .ok()
        .and_then(|v| Channel::parse(v.trim()))
    {
        return (c, true);
    }
    if let Some(c) = overrides(hub)
        .get("channel")
        .and_then(Value::as_str)
        .and_then(|s| Channel::parse(s.trim()))
    {
        return (c, true);
    }
    (
        if repo_available { Channel::Source } else { Channel::Release },
        false,
    )
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

/// The `.AppImage` this process was launched from, when it was launched from
/// one.
///
/// Inside an AppImage `current_exe` points into the throwaway mount
/// (`/tmp/.mount_Threadxxxxx/usr/bin/threadknot`), which is gone the moment the
/// process ends — restarting *that* path relaunches nothing, and comparing its
/// mtime says nothing about whether an update landed. The runtime exports
/// `APPIMAGE` for exactly this reason: it is the real file on disk, the thing a
/// release install replaces, and the thing a restart must exec.
fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// Executable a restart should launch: the freshly built release binary when
/// the checkout has one, otherwise whatever is running now. Checking the build
/// output rather than only `current_exe` is what lets a dev running the debug
/// binary still be told a new release binary is waiting.
fn restart_target(repo: Option<&Path>) -> PathBuf {
    // An AppImage is never built from a checkout, so this settles it outright.
    if let Some(p) = appimage_path() {
        return p;
    }
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

// ---- published releases ---------------------------------------------------

/// Where published builds come from. Overridable so a fork, or a private
/// mirror, updates from its own releases rather than from ours.
fn release_repo() -> String {
    std::env::var("THREADKNOT_RELEASE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASE_REPO.to_string())
}

/// A GitHub token for reading releases, when the repository is not public.
///
/// Read from its own variable rather than from an ambient `GITHUB_TOKEN`: this
/// process inherits the environment of whatever launched it, and a token that
/// happens to be lying around in a shell is not consent to send it to
/// api.github.com. Unset in the shipping configuration, where the repository is
/// public and no credential is involved in updating at all.
fn release_token() -> Option<String> {
    std::env::var("THREADKNOT_RELEASE_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A request to the releases API, with the headers GitHub requires — plus the
/// token when one is configured. Shared by the check and the download so they
/// cannot disagree about who is asking.
fn release_request(client: &reqwest::Client, url: &str, accept: &str) -> reqwest::RequestBuilder {
    let req = client
        .get(url)
        // GitHub rejects a request with no user agent outright.
        .header("User-Agent", format!("threadknot/{}", env!("THREADKNOT_VERSION")))
        .header("Accept", accept)
        .header("X-GitHub-Api-Version", "2022-11-28");
    match release_token() {
        // reqwest drops this on a cross-host redirect, which is exactly right:
        // an asset download ends up at a pre-signed storage URL that rejects a
        // second set of credentials.
        Some(t) => req.header("Authorization", format!("Bearer {t}")),
        None => req,
    }
}

/// How this copy of Threadknot was installed, which decides what "replace
/// yourself" means — and whether it is possible at all.
/// Every variant exists on every platform so the type, the matches and the
/// tests stay one shape; only one of them is ever constructible on a given
/// build, which is what the allow is for.
#[allow(dead_code)]
enum InstallShape {
    /// A portable `.AppImage`: one file, replaced in place.
    AppImage(PathBuf),
    /// A macOS `.app` bundle, replaced wholesale from the disk image.
    MacApp(PathBuf),
    /// Windows: hand the job to the installer that ships in the release.
    WindowsInstaller,
}

/// Work out the install shape, or explain why this copy cannot replace itself.
///
/// The refusals are deliberate rather than best-effort. A `.deb`/`.rpm` install
/// puts Threadknot under `/usr`, owned by the package manager: writing there
/// behind its back leaves the package database describing a file that no longer
/// exists, and the next `apt upgrade` silently reverts the update. Root is not
/// the missing piece — being the wrong tool for the job is.
fn install_shape() -> Result<InstallShape, String> {
    #[cfg(target_os = "macos")]
    {
        // …/Threadknot.app/Contents/MacOS/Threadknot -> …/Threadknot.app
        match current_exe_path()
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(Path::to_path_buf)
        {
            Some(app) => Ok(InstallShape::MacApp(app)),
            None => Err("This copy is running as a bare executable rather than from \
                         Threadknot.app, so it cannot replace itself. Download the disk image \
                         and drag Threadknot to Applications."
                .into()),
        }
    }
    #[cfg(windows)]
    {
        Ok(InstallShape::WindowsInstaller)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match appimage_path() {
            Some(p) => Ok(InstallShape::AppImage(p)),
            None => Err("This copy was installed from a .deb or .rpm package, or built from \
                         source, so its files belong to something else — a package manager, or \
                         your checkout — and Threadknot will not write over them. Update it the \
                         way you installed it, or switch to the portable AppImage."
                .into()),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err("Installing a published build is not wired up on this platform yet.".into())
    }
}

/// Whether an asset filename is the artifact this install shape wants.
///
/// Matching is on shape first and CPU architecture second, and an asset that
/// names *no* architecture is accepted only when it is the sole candidate:
/// a release carrying both an arm64 and an x64 disk image must never install
/// the wrong one just because the names drifted.
fn asset_matches(shape: &InstallShape, name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    match shape {
        InstallShape::AppImage(_) => lower.ends_with(".appimage"),
        InstallShape::MacApp(_) => lower.ends_with(".dmg"),
        // Tauri's NSIS bundle is `<name>_<version>_<arch>-setup.exe`. The bare
        // `threadknot.exe` in the same release is the portable binary, which is
        // not something to run as an installer.
        InstallShape::WindowsInstaller => lower.ends_with("-setup.exe"),
    }
}

/// Architecture tokens that identify a build for the CPU we are running on.
fn arch_tokens() -> &'static [&'static str] {
    match std::env::consts::ARCH {
        // Every spelling the three bundlers use: cargo's triple, Tauri's NSIS
        // suffix, and Debian's architecture name all differ for one CPU.
        "x86_64" => &["x86_64", "x64", "amd64"],
        "aarch64" => &["aarch64", "arm64"],
        "arm" => &["armhf", "armv7"],
        "x86" => &["i686", "i386", "x86"],
        _ => &[],
    }
}

/// Pick the one artifact this machine installs, from everything a release
/// carries.
fn pick_asset<'a>(
    shape: &InstallShape,
    assets: &'a [(String, String, u64)],
) -> Result<&'a (String, String, u64), String> {
    let candidates: Vec<&(String, String, u64)> =
        assets.iter().filter(|(name, _, _)| asset_matches(shape, name)).collect();
    if candidates.is_empty() {
        return Err(format!(
            "This release has no build for {} {}. Check the release page for one.",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    }
    let tokens = arch_tokens();
    let for_us: Vec<&&(String, String, u64)> = candidates
        .iter()
        .filter(|(name, _, _)| {
            let lower = name.to_ascii_lowercase();
            tokens.iter().any(|t| lower.contains(t))
        })
        .collect();
    match (for_us.len(), candidates.len()) {
        (1, _) => Ok(for_us[0]),
        // Nothing names an architecture, and there is only one thing it could
        // be. Older releases predate the arch suffix.
        (0, 1) => Ok(candidates[0]),
        (0, _) => Err(format!(
            "This release has {} builds for this platform but none marked {}, so Threadknot \
             cannot tell which one is for this machine.",
            candidates.len(),
            std::env::consts::ARCH
        )),
        (n, _) => Err(format!(
            "This release has {n} builds marked {}, so Threadknot cannot tell which one to \
             install.",
            std::env::consts::ARCH
        )),
    }
}

/// Ask GitHub for the newest published release and resolve it down to the one
/// artifact this machine would install.
///
/// Unauthenticated in the shipping configuration: the releases of a public
/// repository need no credential, and an updater that demands one is an updater
/// most people never run. That caps us at 60 calls an hour per address, which a
/// 30-minute poll spends 0.5 of.
async fn fetch_latest_release() -> Result<ReleaseInfo> {
    let repo = release_repo();
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let client = reqwest::Client::builder().timeout(RELEASE_TIMEOUT).build()?;
    let body: Value = release_request(&client, &url, "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("could not reach {url}"))?
        .error_for_status()
        .with_context(|| {
            format!(
                "{repo} has no published releases, or GitHub refused (a private repository \
                 answers 404 without a THREADKNOT_RELEASE_TOKEN)"
            )
        })?
        .json()
        .await
        .context("the releases API returned something that is not JSON")?;

    let tag = body
        .get("tag_name")
        .and_then(Value::as_str)
        .context("the latest release has no tag")?
        .to_string();
    // The API's own asset URL, not `browser_download_url`: it redirects to the
    // same storage either way, and it is the only one that works when the
    // repository is private. Public downloads are unaffected.
    let assets: Vec<(String, String, u64)> = body
        .get("assets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some((
                        v.get("name")?.as_str()?.to_string(),
                        v.get("url")?.as_str()?.to_string(),
                        v.get("size").and_then(Value::as_u64).unwrap_or(0),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    // Two independent reasons this machine may not be able to install: it is
    // the wrong kind of install, or the release has nothing for it. Both end up
    // in the same field, because the card treats them the same way — show the
    // release, offer the download link, do not offer the button.
    let (asset, blocked) = match install_shape() {
        Err(why) => (None, Some(why)),
        Ok(shape) => match pick_asset(&shape, &assets) {
            Ok(a) => (Some(a.clone()), None),
            Err(why) => (None, Some(why)),
        },
    };

    let notes = body.get("body").and_then(Value::as_str).unwrap_or_default();
    Ok(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        tag: tag.clone(),
        name: body
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&tag)
            .to_string(),
        // The notes ride on every `updates` broadcast to every connected
        // client, phones included. The card links out for the full text.
        notes: notes.chars().take(1_200).collect(),
        published_at: body
            .get("published_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        url: body
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        asset_name: asset.as_ref().map(|(n, _, _)| n.clone()),
        asset_url: asset.as_ref().map(|(_, u, _)| u.clone()),
        asset_size: asset.as_ref().map(|(_, _, s)| *s).unwrap_or(0),
        blocked,
    })
}

/// Re-read the releases page into the shared snapshot. Failure is not fatal and
/// is not reported: the last good answer stays, and the git side of the status
/// carries its own error field for the things a user can act on.
async fn refresh_release(hub: &Arc<Hub>) {
    match fetch_latest_release().await {
        Ok(info) => hub.updates.store_release(Some(info)),
        Err(e) => tracing::debug!("release check failed: {e:#}"),
    }
}

/// Fold the release snapshot and the channel into a status built from the git
/// side. Every `UpdateStatus` leaving this module goes through here, so the
/// channel is never unset and `update_available` always means "the route this
/// machine actually takes has something newer".
fn with_release(hub: &Arc<Hub>, mut st: UpdateStatus) -> UpdateStatus {
    let (channel, pinned) = resolve_channel(hub, st.repo_available);
    let release = hub.updates.release();
    let running = version_count(&st.running_version).unwrap_or(0);
    // Strictly newer only. A machine building master is routinely *ahead* of
    // the newest release, and offering it a downgrade would be worse than
    // offering it nothing.
    let newer = release
        .as_ref()
        .and_then(|r| version_count(&r.version))
        .is_some_and(|n| n > running);

    st.release_install_supported = release.as_ref().is_some_and(|r| r.asset_url.is_some());
    st.release_available = newer;
    st.channel = channel.as_str().to_string();
    st.channel_pinned = pinned;
    st.release = release;

    if channel == Channel::Release {
        st.update_available = newer;
        // "No git checkout found here" is the normal, correct state of a
        // machine that updates from published builds. Leaving it in the error
        // field puts a red box on a card that has nothing wrong with it.
        if !st.repo_available {
            st.error = None;
        }
    }
    st
}

// ---- status ---------------------------------------------------------------

/// Read the repo's update situation. `fetch` performs a network fetch first;
/// callers that only want to render cached refs pass false.
pub async fn compute(hub: &Arc<Hub>, fetch: bool) -> UpdateStatus {
    // The releases page is checked on the same schedule as the git fetch, and
    // for the same machines: a development box that is pinned to source still
    // wants to know a release shipped, and the fleet list shows both.
    if fetch {
        refresh_release(hub).await;
    }
    let base = match resolve_repo(hub) {
        None => {
            let tried = candidates(hub);
            let detail = if tried.is_empty() {
                "no candidate paths were available".to_string()
            } else {
                format!("looked in — {}", tried.join("; "))
            };
            UpdateStatus::unavailable(
                hub,
                format!(
                    "No Threadknot git checkout found on this machine, so it cannot build \
                     master ({detail}). Set the source folder below to fix this."
                ),
                false,
            )
        }
        Some((repo, source)) => match compute_in(hub, &repo, source, fetch).await {
            Ok(st) => st,
            Err(e) => UpdateStatus::unavailable(hub, format!("{e:#}"), true),
        },
    };
    with_release(hub, base)
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
        // All four are `with_release`'s to fill in; every path out of this
        // module runs through it.
        channel: Channel::Source.as_str().to_string(),
        channel_pinned: false,
        release: None,
        release_available: false,
        release_install_supported: false,
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
        restart_supported: SELF_SERVICE,
        rebuild_supported: SELF_SERVICE,
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
///
/// Returns whether commits actually moved. `require_work` decides what "nothing
/// to pull" means: on its own the pull button was clicked to do something, so
/// silence would look broken; inside the chained run it is the ordinary case of
/// a checkout that is already current with only a stale binary left to fix, and
/// stopping there would skip the rebuild that is the actual point.
async fn fast_forward(
    hub: &Arc<Hub>,
    repo: &Path,
    source: &str,
    require_work: bool,
) -> Result<bool> {
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
    if st.head_behind == 0 {
        anyhow::ensure!(
            !require_work,
            "Already up to date with master; only a rebuild is needed."
        );
        return Ok(false);
    }
    // `--ff-only` is the whole safety story: git itself refuses if the history
    // diverged, so there is no window where we could merge or rebase by accident.
    crate::git::run_git(repo, &["merge", "--ff-only", "origin/master"])
        .await
        .context("fast-forward to origin/master failed")?;
    Ok(true)
}

// ---- rebuild --------------------------------------------------------------

/// The program a build stage should actually spawn.
///
/// Windows ships npm as `npm.cmd`, and `CreateProcess` cannot find that from the
/// bare name — std only ever appends `.exe` — so the frontend stage died with
/// "could not start npm, is it installed?" on a machine where npm was installed
/// and on PATH. Resolving against the same augmented PATH the agent CLIs use
/// fixes that, and also finds a Node that exists only for this user (nvm, Volta),
/// which a GUI-launched app does not otherwise inherit. Left alone everywhere
/// else, where the bare name has always been right.
fn build_program(program: &str) -> std::ffi::OsString {
    #[cfg(windows)]
    if let Some(p) = crate::agents::resolve_bin(program) {
        return p.into_os_string();
    }
    program.into()
}

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
    let mut cmd = tokio::process::Command::new(build_program(program));
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

/// The build output path for this executable's name inside `repo`, whether or
/// not anything has been built there yet. `restart_target` deliberately falls
/// back to the running binary when the file is missing, which is the wrong
/// answer for "what is the linker about to write".
#[cfg(any(windows, test))]
fn build_output(repo: &Path) -> PathBuf {
    let name = current_exe_path().file_name().unwrap_or_default().to_owned();
    repo.join("src-tauri").join("target").join("release").join(name)
}

/// Windows will not let a running executable be overwritten, so a rebuild whose
/// output path is the file this process was launched from dies at the link step
/// with an access error that reads like a broken toolchain. Renaming a running
/// image *is* allowed (only unlinking is not), so move ourselves aside and hand
/// the path back. The leftover is swept on the next rebuild, by which time
/// nothing has it open.
///
/// Normally this is a no-op: a Windows restart relaunches from a copy outside
/// the checkout precisely so the build directory stays free. It matters for the
/// first self-rebuild on a machine that was started straight out of
/// `target\release`, which would otherwise be permanently unable to update
/// itself.
#[cfg(windows)]
fn free_build_target(repo: &Path) {
    let target = build_output(repo);
    let aside = target.with_extension("exe.old");
    let _ = std::fs::remove_file(&aside);
    if !target.is_file() {
        return;
    }
    let current = current_exe_path();
    let same = match (current.canonicalize(), target.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => current == target,
    };
    if same {
        let _ = std::fs::rename(&target, &aside);
    }
}

/// What a finished build left behind.
enum Built {
    /// A binary newer than this process is waiting at this path.
    Fresh(PathBuf),
    /// The build succeeded but produced nothing newer to restart into.
    Unchanged(PathBuf),
}

/// Rebuild the release binary in place, in this process, with the exit code
/// and log captured. Deliberately NOT a detached fire-and-forget: the previous
/// shape reported success the instant the shell started, so a failing compile
/// looked identical to a good one.
///
/// Overwriting the running executable is safe on Unix because cargo renames the
/// new file into place and this process keeps the old inode until it exits.
/// That is precisely why the restart is a separate, explicit step.
///
/// Split out from `run_rebuild` so the chained pull → build → restart run walks
/// the identical stages and produces the identical error text; two copies of a
/// build pipeline drift, and the copy that drifts is the one nobody clicks.
/// Errors come back already formatted (log tail included) because the caller
/// decides which stage name to file them under.
async fn rebuild_core(hub: &Arc<Hub>, repo: &Path, op_id: &str) -> Result<Built, String> {
    let log = repo.join("src-tauri").join("target").join("threadknot-rebuild.log");
    let _ = std::fs::create_dir_all(log.parent().unwrap_or(repo));
    let _ = std::fs::write(&log, format!("==== rebuild {} ====\n", now_iso()));
    hub.updates.set_log(op_id, &log);

    #[cfg(windows)]
    free_build_target(repo);

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
        hub.updates.stage(op_id, stage);
        refresh(hub).await;
        if let Err(e) = build_stage(repo, &log, program, &args).await {
            return Err(format!("{e:#}\n\n{}", log_tail(&log)));
        }
    }

    hub.updates.stage(op_id, "verifying binary");
    refresh(hub).await;
    let target = restart_target(Some(repo));
    if !target.is_file() {
        return Err(format!("the build finished but {} does not exist", target.display()));
    }
    // The build can "succeed" without producing anything new (nothing to do,
    // or output written somewhere else). Saying "restart to load it" then would
    // send the user in a circle.
    Ok(match binary_is_newer(&target) {
        true => Built::Fresh(target),
        false => Built::Unchanged(target),
    })
}

/// Why a build that changed nothing is a dead end, in words the card can show.
fn unchanged_note(target: &Path) -> String {
    format!(
        "The build succeeded but {} is not newer than the running process. \
         Nothing to restart into.",
        target.display()
    )
}

async fn run_rebuild(hub: Arc<Hub>, repo: PathBuf, op_id: String) {
    match rebuild_core(&hub, &repo, &op_id).await {
        Err(e) => hub.updates.finish(&op_id, false, "rebuild failed", Some(e)),
        Ok(Built::Unchanged(t)) => hub.updates.finish(
            &op_id,
            true,
            "rebuild complete, binary unchanged",
            Some(unchanged_note(&t)),
        ),
        Ok(Built::Fresh(_)) => {
            hub.updates.finish(&op_id, true, "rebuild complete, restart required", None)
        }
    }
    refresh(&hub).await;
}

// ---- the whole update, in one click ---------------------------------------

/// Pull, rebuild, and swap the running app for the result, as a single
/// operation.
///
/// Chained on the server rather than in the UI for the same reason the rebuild
/// is: a release compile outlives the tab that started it, and every safety
/// rule has to run on the machine that owns the checkout rather than on the one
/// that clicked. A failure at any stage stops the chain and leaves the ordinary
/// per-step buttons to finish the job by hand.
async fn run_update(hub: Arc<Hub>, repo: PathBuf, source: &'static str, op_id: String, force: bool) {
    hub.updates.stage(&op_id, "pulling");
    refresh(&hub).await;
    if let Err(e) = fast_forward(&hub, &repo, source, false).await {
        hub.updates.finish(&op_id, false, "pull failed", Some(format!("{e:#}")));
        refresh(&hub).await;
        hub.updates.kick(true);
        return;
    }

    let target = match rebuild_core(&hub, &repo, &op_id).await {
        Ok(Built::Fresh(t)) => t,
        Ok(Built::Unchanged(t)) => {
            hub.updates
                .finish(&op_id, true, "pulled, binary unchanged", Some(unchanged_note(&t)));
            refresh(&hub).await;
            hub.updates.kick(true);
            return;
        }
        Err(e) => {
            hub.updates.finish(&op_id, false, "rebuild failed", Some(e));
            refresh(&hub).await;
            hub.updates.kick(true);
            return;
        }
    };

    // The active-work guard is not spent just because a build preceded it. A
    // release compile runs for minutes, and threads that were idle at click time
    // are routinely mid-turn by the end of it — so the check has to happen here,
    // against the fleet as it is now, not as it was when the button was pressed.
    // Stopping leaves the plain restart button armed, so nothing is lost except
    // the automation.
    let busy = active_work(&hub);
    if busy > 0 && !force {
        hub.updates.finish(
            &op_id,
            true,
            "built, waiting to restart",
            Some(format!(
                "{busy} thread(s) started working while this built, so Threadknot left \
                 itself running. Use restart now once they finish."
            )),
        );
        refresh(&hub).await;
        hub.updates.kick(true);
        return;
    }

    hub.updates.stage(&op_id, "restarting");
    refresh(&hub).await;
    match spawn_restart(&target, Some(&repo)) {
        Ok(()) => {
            // Nothing after this is guaranteed to run.
            hub.updates.finish(&op_id, true, "restarting", None);
            stand_down();
        }
        Err(e) => {
            hub.updates.finish(&op_id, false, "restart failed", Some(format!("{e:#}")));
            refresh(&hub).await;
        }
    }
}

// ---- installing a published release ---------------------------------------

/// Publish a stage change without recomputing the git side.
///
/// `refresh` shells out to git several times, which is the right cost once per
/// operation and the wrong one twenty times during a download. Clients read the
/// live operation off `git.selfUpdateStatus` anyway, so the broadcast alone is
/// enough to move a progress bar.
fn announce(hub: &Arc<Hub>) {
    hub.broadcast_state("updates", None);
}

/// Somewhere to put a downloaded artifact: machine-local, wiped before each
/// use so a failed run cannot leave half an installer to be picked up later.
fn download_dir(hub: &Arc<Hub>) -> PathBuf {
    let dir = hub.store.dir().join("updates");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Stream one release artifact to disk, reporting progress as a stage.
///
/// Streamed rather than buffered because these are installers: holding a
/// hundred megabytes in memory to write it straight back out is a cost paid for
/// nothing, on the machines least able to pay it.
async fn download_asset(
    hub: &Arc<Hub>,
    url: &str,
    dest: &Path,
    expected: u64,
    op_id: &str,
) -> Result<()> {
    use futures_util::StreamExt;
    use std::io::Write;

    anyhow::ensure!(
        expected <= MAX_ASSET_BYTES,
        "{url} claims to be {expected} bytes, which is far larger than a Threadknot build. \
         Refusing to download it."
    );

    let client = reqwest::Client::builder().timeout(DOWNLOAD_TIMEOUT).build()?;
    // `octet-stream` is what turns the API's asset URL from a JSON description
    // of the file into the file.
    let resp = release_request(&client, url, "application/octet-stream")
        .send()
        .await
        .with_context(|| format!("could not download {url}"))?
        .error_for_status()
        .with_context(|| format!("{url} refused the download"))?;

    let total = resp.content_length().unwrap_or(expected);
    let mut file = std::fs::File::create(dest)
        .with_context(|| format!("cannot write {}", dest.display()))?;
    let mut stream = resp.bytes_stream();
    let mut done: u64 = 0;
    let mut announced = 0u64;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the download was cut short")?;
        done += chunk.len() as u64;
        anyhow::ensure!(
            done <= MAX_ASSET_BYTES,
            "the download exceeded {MAX_ASSET_BYTES} bytes and was stopped"
        );
        file.write_all(&chunk).context("writing the download to disk failed")?;
        let pct = if total > 0 { done * 100 / total } else { 0 };
        // Every chunk is a websocket frame to every connected client, phones
        // included. Five points at a time is a moving bar and a quiet network.
        if pct >= announced + 5 {
            announced = pct;
            hub.updates.stage(op_id, &format!("downloading {pct}%"));
            announce(hub);
        }
    }
    file.flush().context("writing the download to disk failed")?;
    drop(file);

    // The size GitHub reported is not a signature, but it is a free check that
    // what landed is what was offered — and it catches the truncated download,
    // which is the failure that would otherwise be discovered by installing it.
    if expected > 0 {
        let got = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        anyhow::ensure!(
            got == expected,
            "the download is {got} bytes but the release says {expected}. \
             It was truncated or altered in transit; nothing was installed."
        );
    }
    Ok(())
}

/// Replace this install with the downloaded artifact, and return the executable
/// a restart should launch.
fn install_downloaded(shape: &InstallShape, downloaded: &Path) -> Result<PathBuf> {
    match shape {
        #[cfg(all(unix, not(target_os = "macos")))]
        InstallShape::AppImage(target) => {
            install_appimage(downloaded, target)?;
            Ok(target.clone())
        }
        #[cfg(target_os = "macos")]
        InstallShape::MacApp(app) => {
            install_dmg(downloaded, app)?;
            // Exec the binary inside the bundle rather than `open`ing it: the
            // restart helper below is one shared code path, and this keeps the
            // relaunched app in the same session as everything else.
            Ok(app.join("Contents").join("MacOS").join(
                current_exe_path().file_name().unwrap_or_else(|| std::ffi::OsStr::new("Threadknot")),
            ))
        }
        #[cfg(windows)]
        InstallShape::WindowsInstaller => {
            // Windows cannot overwrite a running image, so the installer runs
            // after we are gone and relaunches us itself. Nothing is left for
            // the ordinary restart path to do.
            spawn_install_windows(downloaded)?;
            Ok(current_exe_path())
        }
        // Every arm above is behind a cfg, so on any one platform the other
        // variants are unconstructible — but the match still has to be total.
        #[allow(unreachable_patterns)]
        _ => anyhow::bail!("this install shape cannot replace itself on this platform"),
    }
}

/// Swap in a new AppImage: stage it beside the old one, make it executable,
/// then rename over the top.
///
/// Staged in the destination directory rather than moved from the download
/// directory because those are routinely different filesystems, where a rename
/// is not atomic but a failure. The rename itself is what makes the swap
/// all-or-nothing: at no point is there a partially written file at the path
/// the desktop entry launches.
#[cfg(all(unix, not(target_os = "macos")))]
fn install_appimage(downloaded: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let staged = target.with_extension("appimage-new");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(downloaded, &staged).with_context(|| {
        format!(
            "cannot write {} — Threadknot needs write access to the folder it runs from",
            staged.display()
        )
    })?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .context("could not make the new AppImage executable")?;
    std::fs::rename(&staged, target).with_context(|| {
        format!("could not replace {} with the new build", target.display())
    })?;
    Ok(())
}

/// Run a helper program to completion, with its output folded into the error.
#[cfg(target_os = "macos")]
fn run_tool(program: &str, args: &[&std::ffi::OsStr]) -> Result<()> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run {program}"))?;
    anyhow::ensure!(
        out.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Mount the disk image, stage the bundle it carries beside the installed one,
/// then swap them.
///
/// `ditto` rather than a recursive copy: it is the only tool that reliably
/// preserves the extended attributes and symlink layout a signed `.app`
/// depends on, and a bundle copied without them fails Gatekeeper on launch —
/// which looks exactly like a corrupt download.
#[cfg(target_os = "macos")]
fn install_dmg(dmg: &Path, app: &Path) -> Result<()> {
    use std::ffi::OsStr;

    let mount = std::env::temp_dir().join(format!("threadknot-dmg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&mount).context("could not make a mount point")?;
    run_tool(
        "hdiutil",
        &[
            OsStr::new("attach"),
            OsStr::new("-nobrowse"),
            OsStr::new("-readonly"),
            OsStr::new("-mountpoint"),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )
    .context("could not open the downloaded disk image")?;

    let swap = (|| -> Result<()> {
        let src = std::fs::read_dir(&mount)
            .context("could not read the disk image")?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .context("the disk image contains no application bundle")?;

        let staged = app.with_extension("app.new");
        let previous = app.with_extension("app.old");
        let _ = std::fs::remove_dir_all(&staged);
        let _ = std::fs::remove_dir_all(&previous);
        run_tool("ditto", &[src.as_os_str(), staged.as_os_str()])
            .context("could not copy the new build out of the disk image")?;

        // From here the swap is two renames on one directory: fast, and with
        // only one instant where the destination is absent. If the second one
        // fails, put the old bundle back rather than leaving nothing installed.
        std::fs::rename(app, &previous)
            .with_context(|| format!("could not move {} aside", app.display()))?;
        if let Err(e) = std::fs::rename(&staged, app) {
            let _ = std::fs::rename(&previous, app);
            return Err(e).with_context(|| format!("could not install the new {}", app.display()));
        }
        let _ = std::fs::remove_dir_all(&previous);
        Ok(())
    })();

    let _ = run_tool("hdiutil", &[OsStr::new("detach"), mount.as_os_str(), OsStr::new("-force")]);
    let _ = std::fs::remove_dir(&mount);
    swap
}

/// Hand the release's own installer the job, from a detached helper that
/// outlives us.
///
/// Windows holds a running executable open, so the installer cannot touch this
/// process's files while it exists — and the installer is also the only thing
/// that can keep the Start Menu shortcut, and with it the AppUserModelID that
/// native toasts require, consistent with what is on disk. So the helper waits
/// for us to go, runs it silently, and starts whatever it installed. It appends
/// to the same log the restart helper uses, so one file tells the whole story
/// when an update does not come back.
#[cfg(windows)]
fn spawn_install_windows(installer: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;

    let current = current_exe_path();
    let fallback = live_exe(&current);
    let script = format!(
        "$ErrorActionPreference = 'SilentlyContinue'\n\
         $log = Join-Path $env:TEMP 'threadknot-restart.log'\n\
         function Say($m) {{ \"$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')  $m\" | Add-Content $log }}\n\
         Say '==================== release install begin ===================='\n\
         $setup = {setup}\n\
         $current = {current}\n\
         $fallback = {fallback}\n\
         try {{ Wait-Process -Id {pid} -Timeout 60 }} catch {{}}\n\
         if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Say 'still running, forcing'; Stop-Process -Id {pid} -Force; Start-Sleep -Seconds 2 }}\n\
         Say \"old pid gone: {pid}\"\n\
         $p = Start-Process -FilePath $setup -ArgumentList '/S' -Wait -PassThru\n\
         Say \"installer exit: $($p.ExitCode)\"\n\
         if ($p.ExitCode -ne 0) {{ Say 'FAIL: the installer reported an error'; Remove-Item $PSCommandPath -Force; exit 1 }}\n\
         $exe = $null\n\
         if (Test-Path $current) {{ $exe = $current }} elseif (Test-Path $fallback) {{ $exe = $fallback }}\n\
         if ($exe) {{ Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe); Say \"launched $exe\" }}\n\
         else {{ Say 'installed, but could not find the executable to launch' }}\n\
         Remove-Item $PSCommandPath -Force\n",
        setup = ps_quote(&installer.display().to_string()),
        current = ps_quote(&current.display().to_string()),
        fallback = ps_quote(&fallback.display().to_string()),
        pid = std::process::id(),
    );
    debug_assert!(script.is_ascii(), "the install helper must stay ASCII-only");

    let path = std::env::temp_dir().join(format!("threadknot-install-{}.ps1", uuid::Uuid::new_v4()));
    std::fs::write(&path, &script)
        .with_context(|| format!("could not write the install helper to {}", path.display()))?;
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED)
        .spawn()
        .context("could not start the install helper")?;
    Ok(())
}

/// Download the published release, install it over this copy, and relaunch —
/// the whole thing off one click, as an app that updates itself should.
///
/// Server-side for the same reasons the rebuild is: the download outlives the
/// tab that started it, and every rule about whether this machine may replace
/// itself has to run on the machine being replaced, not on the one that
/// clicked.
async fn run_install(hub: Arc<Hub>, release: ReleaseInfo, op_id: String, force: bool) {
    let fail = |stage: &str, e: String| {
        hub.updates.finish(&op_id, false, stage, Some(e));
    };

    let (Some(url), Some(name)) = (release.asset_url.clone(), release.asset_name.clone()) else {
        fail(
            "install failed",
            release.blocked.clone().unwrap_or_else(|| {
                "This release has nothing this machine can install.".to_string()
            }),
        );
        refresh(&hub).await;
        return;
    };

    hub.updates.stage(&op_id, "downloading");
    announce(&hub);
    let dest = download_dir(&hub).join(&name);
    if let Err(e) = download_asset(&hub, &url, &dest, release.asset_size, &op_id).await {
        fail("download failed", format!("{e:#}"));
        refresh(&hub).await;
        return;
    }

    // Checked here rather than at click time, exactly as the chained rebuild
    // does: a download runs for minutes, and threads that were idle when the
    // button was pressed are routinely mid-turn by the end of it. The new build
    // is already on disk, so nothing is lost by stopping — the restart button
    // finishes the job whenever the user is ready.
    let busy = active_work(&hub);
    if busy > 0 && !force {
        hub.updates.finish(
            &op_id,
            true,
            "downloaded, waiting to install",
            Some(format!(
                "{busy} thread(s) started working while this downloaded, so Threadknot left \
                 itself alone. Run the update again once they finish."
            )),
        );
        refresh(&hub).await;
        return;
    }

    hub.updates.stage(&op_id, "installing");
    announce(&hub);
    let shape = match install_shape() {
        Ok(s) => s,
        Err(why) => {
            fail("install failed", why);
            refresh(&hub).await;
            return;
        }
    };
    let target = match install_downloaded(&shape, &dest) {
        Ok(t) => t,
        Err(e) => {
            fail("install failed", format!("{e:#}"));
            refresh(&hub).await;
            return;
        }
    };
    let _ = std::fs::remove_file(&dest);

    // Windows' installer helper is already counting down to kill us and will
    // relaunch on its own; signalling ourselves as well would race it.
    if matches!(shape, InstallShape::WindowsInstaller) {
        hub.updates.finish(&op_id, true, "installing and restarting", None);
        announce(&hub);
        stand_down();
        return;
    }

    hub.updates.stage(&op_id, "restarting");
    announce(&hub);
    match spawn_restart(&target, None) {
        Ok(()) => {
            // Nothing after this is guaranteed to run.
            hub.updates.finish(&op_id, true, "restarting", None);
            stand_down();
        }
        Err(e) => {
            hub.updates.finish(
                &op_id,
                false,
                "restart failed",
                Some(format!(
                    "{e:#} — v{} is installed, so closing and reopening Threadknot loads it.",
                    release.version
                )),
            );
            refresh(&hub).await;
        }
    }
}

// ---- restart --------------------------------------------------------------

/// Replace the running app with the executable on disk.
///
/// Targets exactly this process by pid. Nothing here matches on a process name,
/// so no unrelated Threadknot, Claude, Codex, terminal or editor can be caught by
/// it. The child is session-detached so killing us cannot take it down, and it
/// inherits our environment, which is what keeps the relaunched window attached
/// to the same desktop session.
///
/// `repo` is only used by the Windows path, which has to ship the web bundle
/// next to the copy it launches.
fn spawn_restart(target: &Path, repo: Option<&Path>) -> Result<()> {
    anyhow::ensure!(
        target.is_file(),
        "nothing to restart into: {} does not exist",
        target.display()
    );
    #[cfg(unix)]
    {
        let _ = repo;
        spawn_restart_unix(target)
    }
    #[cfg(windows)]
    {
        spawn_restart_windows(target, repo)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = repo;
        anyhow::bail!(
            "Automatic restart is not wired up on this platform yet. Close Threadknot and \
             open it again to load the new build."
        )
    }
}

/// Once the helper is armed, get out of its way.
///
/// Unix does not need this: the helper's first act is a TERM, which is the
/// app's normal shutdown path. Windows has no such signal to send, and the
/// helper's only other lever is a hard kill once its grace period expires — so
/// standing down deliberately is both faster and gentler. The delay is there to
/// let the response frame reach whoever asked before the socket dies with us.
fn stand_down() {
    #[cfg(windows)]
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        std::process::exit(0);
    });
}

#[cfg(unix)]
fn spawn_restart_unix(target: &Path) -> Result<()> {
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

#[cfg(unix)]
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// Single-quote for `sh -c`, escaping embedded quotes.
#[cfg(unix)]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Single-quote for PowerShell, where the escape for a literal quote is to
/// double it. Every path below goes through this, because the ones that matter
/// on this platform live under `C:\Users\...\OneDrive\Desktop\...` and are full
/// of spaces.
#[cfg(windows)]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Where Windows runs Threadknot from once it has restarted itself.
///
/// Windows locks a running executable, so the app cannot live at the path the
/// linker needs to write. Every restart therefore copies the fresh build to a
/// stable spot outside the checkout and launches that, which is what keeps the
/// *next* rebuild — and any `npx tauri build` run by hand in a terminal — from
/// failing on a locked file. `scripts/restart-windows.ps1` has always done this;
/// `THREADKNOT_LIVE_EXE` overrides it in both, so a machine that already runs
/// from somewhere else keeps running from there.
#[cfg(windows)]
fn live_exe(target: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("THREADKNOT_LIVE_EXE").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let name = target
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("threadknot.exe"))
        .to_owned();
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Threadknot")
        .join(name)
}

/// Windows restart: a detached PowerShell helper that outlives us, waits for
/// this pid to go, refreshes the live copy from the fresh build, and launches
/// it.
///
/// The script is written to a temp file rather than passed with `-Command`
/// because Windows hands a process one flat command-line string, and the paths
/// involved here are neither short nor free of spaces. It appends to the same
/// `%TEMP%\threadknot-restart.log` the shell script uses, so both routes leave
/// one trail to read when a swap does not come back.
#[cfg(windows)]
fn spawn_restart_windows(target: &Path, repo: Option<&Path>) -> Result<()> {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: no console window, and no
    // dependency on the process that spawned it — which is about to be killed
    // by the very script being spawned.
    const DETACHED: u32 = 0x0000_0008 | 0x0000_0200;

    let live = live_exe(target);
    // The server resolves the LAN/phone UI from a dist folder near the exe; the
    // live copy runs far from the checkout, so without this every launch serves
    // the "Web UI not built yet" fallback.
    let dist = repo.map(|r| r.join("dist")).unwrap_or_default();

    let script = format!(
        "$ErrorActionPreference = 'SilentlyContinue'\n\
         $log = Join-Path $env:TEMP 'threadknot-restart.log'\n\
         function Say($m) {{ \"$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')  $m\" | Add-Content $log }}\n\
         Say '==================== self-restart begin ===================='\n\
         $target = {target}\n\
         $live = {live}\n\
         $dist = {dist}\n\
         try {{ Wait-Process -Id {pid} -Timeout 30 }} catch {{}}\n\
         if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Say 'still running, forcing'; Stop-Process -Id {pid} -Force; Start-Sleep -Seconds 2 }}\n\
         Say \"old pid gone: {pid}\"\n\
         if ($target -ne $live) {{\n\
         \x20 New-Item -ItemType Directory -Force -Path (Split-Path $live) | Out-Null\n\
         \x20 $copied = $false\n\
         \x20 for ($i = 0; $i -lt 20; $i++) {{ try {{ Copy-Item $target $live -Force -ErrorAction Stop; $copied = $true; break }} catch {{ Start-Sleep -Milliseconds 500 }} }}\n\
         \x20 Say \"binary copy: $copied\"\n\
         \x20 if (-not $copied) {{ Say 'FAIL: could not replace the live binary'; Remove-Item $PSCommandPath -Force; exit 1 }}\n\
         \x20 if ($dist -and (Test-Path (Join-Path $dist 'index.html'))) {{\n\
         \x20   $dst = Join-Path (Split-Path $live) 'dist'\n\
         \x20   try {{ if (Test-Path $dst) {{ Remove-Item $dst -Recurse -Force -ErrorAction Stop }}; Copy-Item $dist $dst -Recurse -Force -ErrorAction Stop; Say 'web dist copy: ok' }} catch {{ Say 'web dist copy: FAILED' }}\n\
         \x20 }}\n\
         }}\n\
         Start-Process -FilePath $live -WorkingDirectory (Split-Path $live)\n\
         Say \"launched $live\"\n\
         Remove-Item $PSCommandPath -Force\n",
        target = ps_quote(&target.display().to_string()),
        live = ps_quote(&live.display().to_string()),
        dist = ps_quote(&dist.display().to_string()),
        pid = std::process::id(),
    );

    // Keep it ASCII: PowerShell 5.1 reads a BOM-less file as ANSI, and smart
    // punctuation sneaking into this script breaks the parse rather than the
    // wording.
    debug_assert!(script.is_ascii(), "the restart helper must stay ASCII-only");
    let path = std::env::temp_dir().join(format!("threadknot-restart-{}.ps1", uuid::Uuid::new_v4()));
    std::fs::write(&path, &script)
        .with_context(|| format!("could not write the restart helper to {}", path.display()))?;

    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(DETACHED)
        .spawn()
        .context("could not start the restart helper")?;
    Ok(())
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
            | "git.selfUpdateInstall"
            | "git.selfUpdateSetRepoPath"
            | "git.selfUpdateSetChannel"
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
        // `chain: true` asks for the whole job — pull, rebuild, restart — off
        // one click. It is opt-in per request rather than the default because
        // this call is routable: a peer that predates the chain, or one that
        // cannot rebuild itself, must keep doing exactly what it always did.
        // The answer then arrives the way the rebuild's does, on the `updates`
        // broadcast, since a release compile outlives any request.
        "git.selfUpdatePull" => {
            let (repo, source) = require_repo(hub)?;
            let chain =
                SELF_SERVICE && payload.get("chain").and_then(Value::as_bool).unwrap_or(false);
            if chain {
                let force = payload.get("force").and_then(Value::as_bool).unwrap_or(false);
                let op = hub.updates.begin("update")?;
                tokio::spawn(run_update(hub.clone(), repo, source, op.id.clone(), force));
                return Ok(json!({ "ok": true, "operationId": op.id }));
            }
            let op = hub.updates.begin("pull")?;
            hub.updates.stage(&op.id, "pulling");
            let out = fast_forward(hub, &repo, source, true).await;
            match &out {
                Ok(_) => hub.updates.finish(&op.id, true, "repository current", None),
                Err(e) => hub.updates.finish(&op.id, false, "pull failed", Some(format!("{e:#}"))),
            }
            hub.updates.kick(true);
            out.map(|_| json!({ "ok": true }))
        }
        "git.selfUpdateRebuild" => {
            let (repo, _) = require_repo(hub)?;
            anyhow::ensure!(
                SELF_SERVICE,
                "Automatic rebuild is not wired up on this platform yet. \
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
            match spawn_restart(&target, repo.as_deref()) {
                Ok(()) => {
                    // Nothing after this is guaranteed to run: the helper is
                    // already counting down to kill us.
                    hub.updates.finish(&op.id, true, "restarting", None);
                    stand_down();
                    Ok(json!({ "ok": true, "restarting": true }))
                }
                Err(e) => {
                    hub.updates.finish(&op.id, false, "restart failed", Some(format!("{e:#}")));
                    refresh(hub).await;
                    Err(e)
                }
            }
        }
        // Download the newest published release, install it over this copy, and
        // relaunch. Returns as soon as the run is claimed — the download and
        // install outlive any request — so the answer arrives on the `updates`
        // broadcast like the rebuild's does.
        "git.selfUpdateInstall" => {
            let release = hub
                .updates
                .release()
                .context("no published release has been read yet. Use check now first.")?;
            let running = version_count(env!("THREADKNOT_VERSION")).unwrap_or(0);
            anyhow::ensure!(
                version_count(&release.version).is_some_and(|n| n > running),
                "v{} is not newer than the running build (v{}).",
                release.version,
                env!("THREADKNOT_VERSION")
            );
            if let Some(why) = &release.blocked {
                anyhow::bail!("{why}");
            }
            let force = payload.get("force").and_then(Value::as_bool).unwrap_or(false);
            let op = hub.updates.begin("install")?;
            tokio::spawn(run_install(hub.clone(), release, op.id.clone(), force));
            Ok(json!({ "ok": true, "operationId": op.id }))
        }
        // Which route this machine takes to a newer build. Machine-local: a
        // development box and the laptop that only runs releases are different
        // machines with different answers, and neither is the mesh's business.
        "git.selfUpdateSetChannel" => {
            let raw = payload.get("channel").and_then(Value::as_str).unwrap_or("auto").trim();
            match raw {
                // Back to being derived from whether a checkout exists.
                "auto" | "" => write_override(hub, "channel", None)?,
                other => {
                    let c = Channel::parse(other)
                        .with_context(|| format!("unknown update channel: {other}"))?;
                    write_override(hub, "channel", Some(c.as_str()))?;
                }
            }
            hub.updates.kick(true);
            Ok(json!({ "ok": true }))
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

    /// Version comparison is the whole release channel: the tag and the build
    /// number are the same number by construction (`scripts/release.sh`), so a
    /// release is an update exactly when its count is higher. A machine
    /// building master is routinely ahead of the newest release and must never
    /// be offered one.
    #[test]
    fn a_release_is_newer_only_when_its_commit_count_is_higher() {
        let newer = |release: &str, running: &str| {
            version_count(release).unwrap() > version_count(running).unwrap()
        };
        assert!(newer("0.1.92", "0.1.85"));
        assert!(!newer("0.1.92", "0.1.92"));
        assert!(!newer("0.1.85", "0.1.92"));
        // A dev build carrying local commits is still "at" its master count, so
        // a release cut past it is genuinely newer.
        assert!(newer("0.1.92", "0.1.85-dev3"));
        assert!(!newer("0.1.85", "0.1.85-dev3"));
    }

    #[test]
    fn channel_names_round_trip_and_reject_anything_else() {
        for c in [Channel::Release, Channel::Source] {
            assert_eq!(Channel::parse(c.as_str()), Some(c));
        }
        assert_eq!(Channel::parse("auto"), None);
        assert_eq!(Channel::parse(""), None);
    }

    /// Each install shape takes exactly one kind of artifact out of a release
    /// that carries five. Matching a `.deb` as if it were an AppImage, or the
    /// portable `threadknot.exe` as if it were the installer, would download
    /// the wrong file and then run it.
    #[test]
    fn asset_matching_is_per_install_shape() {
        let appimage = InstallShape::AppImage(PathBuf::from("/opt/Threadknot.AppImage"));
        let mac = InstallShape::MacApp(PathBuf::from("/Applications/Threadknot.app"));
        let win = InstallShape::WindowsInstaller;

        assert!(asset_matches(&appimage, "Threadknot_0.1.92_amd64.AppImage"));
        assert!(!asset_matches(&appimage, "Threadknot_0.1.92_amd64.deb"));
        assert!(!asset_matches(&appimage, "threadknot-linux"));

        assert!(asset_matches(&mac, "Threadknot_0.1.92_aarch64.dmg"));
        assert!(!asset_matches(&mac, "Threadknot_0.1.92_amd64.AppImage"));

        assert!(asset_matches(&win, "Threadknot_0.1.92_x64-setup.exe"));
        // The portable binary shares the extension and is not an installer.
        assert!(!asset_matches(&win, "threadknot.exe-windows"));
        assert!(!asset_matches(&win, "threadknot.exe"));
    }

    #[test]
    fn asset_picking_prefers_this_architecture_and_refuses_a_guess() {
        let shape = InstallShape::MacApp(PathBuf::from("/Applications/Threadknot.app"));
        let asset = |n: &str| (n.to_string(), format!("https://example/{n}"), 10u64);

        // The real release shape: two disk images, one per architecture.
        let both = [asset("Threadknot_0.1.92_aarch64.dmg"), asset("Threadknot_0.1.92_x64.dmg")];
        let want = match std::env::consts::ARCH {
            "aarch64" => "Threadknot_0.1.92_aarch64.dmg",
            _ => "Threadknot_0.1.92_x64.dmg",
        };
        assert_eq!(pick_asset(&shape, &both).unwrap().0, want);

        // One unlabelled image is unambiguous, so take it: releases predating
        // the arch suffix are still installable.
        let one = [asset("Threadknot.dmg")];
        assert_eq!(pick_asset(&shape, &one).unwrap().0, "Threadknot.dmg");

        // Two unlabelled images are a coin toss, and installing the wrong
        // architecture produces an app that will not launch.
        let ambiguous = [asset("Threadknot.dmg"), asset("Threadknot-beta.dmg")];
        assert!(pick_asset(&shape, &ambiguous).is_err());

        // Nothing for this platform at all is its own message.
        let none = [asset("Threadknot_0.1.92_amd64.deb")];
        let err = pick_asset(&shape, &none).unwrap_err();
        assert!(err.contains("no build for"), "got: {err}");
    }

    /// The swap that a Linux release install comes down to. The staging file
    /// has to land in the *destination* directory (the download lives on
    /// another filesystem, where a rename is an error rather than an atomic
    /// move), the result has to be executable (a downloaded file is 644, and a
    /// non-executable AppImage is an app that will not start), and nothing may
    /// be left behind for the next run to trip over.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn installing_an_appimage_replaces_it_atomically_and_keeps_it_executable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = Scratch::new("appimage");
        let target = dir.0.join("Threadknot.AppImage");
        std::fs::write(&target, "old build").unwrap();
        let downloaded = dir.0.join("download").join("Threadknot_0.1.93_amd64.AppImage");
        std::fs::create_dir_all(downloaded.parent().unwrap()).unwrap();
        std::fs::write(&downloaded, "new build").unwrap();
        // As it arrives from the network: readable, not runnable.
        std::fs::set_permissions(&downloaded, std::fs::Permissions::from_mode(0o644)).unwrap();

        install_appimage(&downloaded, &target).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new build");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!target.with_extension("appimage-new").exists(), "staging file left behind");
        // The download is left for the caller to clear, not consumed here.
        assert!(downloaded.exists());
    }

    /// Every architecture Threadknot builds for has to be able to name itself,
    /// or `pick_asset` silently falls back to the single-candidate path and
    /// installs whatever happens to be first.
    #[test]
    fn this_architecture_has_tokens_to_match_on() {
        assert!(
            !arch_tokens().is_empty(),
            "no asset-name tokens for {}",
            std::env::consts::ARCH
        );
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

    #[cfg(unix)]
    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("/plain/path"), "'/plain/path'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    /// The bare name is not launchable on Windows for anything npm installs,
    /// and a build stage that cannot start its own compiler reports the machine
    /// has no toolchain. Skipped rather than failed where npm is absent: this
    /// asserts the shape of the resolution, not what is on the box.
    #[cfg(windows)]
    #[test]
    fn build_program_resolves_the_windows_shim_rather_than_the_bare_name() {
        if let Some(p) = crate::agents::resolve_bin("npm") {
            assert_eq!(build_program("npm"), p.into_os_string());
            assert_ne!(build_program("npm"), std::ffi::OsString::from("npm"));
        }
    }

    /// Every path the Windows helper touches is interpolated into the script as
    /// a literal, and they are all full of spaces on a real machine.
    #[cfg(windows)]
    #[test]
    fn ps_quote_doubles_single_quotes() {
        assert_eq!(ps_quote(r"C:\Program Files\tk"), r"'C:\Program Files\tk'");
        assert_eq!(ps_quote("it's"), "'it''s'");
    }

    /// `build_output` has to name the file the linker is about to write even
    /// when nothing has been built yet — that is exactly when Windows needs to
    /// know whether it is standing on it. `restart_target` cannot answer this:
    /// it falls back to the running binary when the output is missing, which
    /// would point the rename at whatever is running instead.
    #[test]
    fn build_output_names_the_release_artifact_before_it_exists() {
        let dir = Scratch::new("output");
        let name = std::env::current_exe().unwrap().file_name().unwrap().to_owned();
        assert_eq!(
            build_output(&dir.0),
            dir.0.join("src-tauri").join("target").join("release").join(&name)
        );
        assert_ne!(build_output(&dir.0), restart_target(Some(&dir.0)));
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
