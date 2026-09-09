//! Multi-repo Git integration. A project is a FOLDER that may contain several
//! git repositories (frontend/, backend/, mobile/, …) — repos are discovered by
//! scanning for `.git` entries and persisted as first-class records keyed by
//! project-relative path, so ids stay stable across restarts. Every operation
//! here is repo-scoped; there is deliberately no "project-wide git status".
//!
//! All operations shell out to the installed `git` CLI (like the agent drivers
//! wrap the installed `claude`/`codex`) so push/pull inherit the user's real
//! SSH keys and credential helpers. `GIT_TERMINAL_PROMPT=0` keeps a missing
//! credential from hanging the server on an interactive prompt.

use crate::protocol::RepoRecord;
use crate::server::ServerState;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Directory names never scanned for nested repos (mirrors files.rs).
const EXCLUDED_DIRS: [&str; 3] = [".git", "node_modules", "target"];
/// How deep below the project root to look for `.git` entries.
const MAX_DEPTH: usize = 4;
/// Safety cap on directories visited during discovery.
const MAX_SCAN_DIRS: usize = 5_000;
/// Unified diffs larger than this come back truncated.
const DIFF_CAP: usize = 256 * 1024;

// ---- discovery -----------------------------------------------------------

/// Is `dir` a *standalone* git repo worth listing? A `.git` entry existing is
/// not enough:
///  - an empty/corrupt `.git` dir (no HEAD) makes `git status` fail — skip;
///  - a `.git` FILE is a `gitdir:` pointer. Pointers into `.git/worktrees/`
///    (linked worktrees) or `.git/modules/` (submodules) are checkouts of a
///    repo that's listed elsewhere — skip them, they are not separate repos.
///    A pointer elsewhere (`--separate-git-dir`) counts if it resolves.
fn is_repo_dir(dir: &Path) -> bool {
    let dotgit = dir.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&dotgit) else {
        return false;
    };
    if meta.is_dir() {
        return dotgit.join("HEAD").exists();
    }
    if meta.is_file() {
        if let Ok(text) = std::fs::read_to_string(&dotgit) {
            if let Some(p) = text.trim().strip_prefix("gitdir:") {
                let p = p.trim();
                if p.contains("/.git/worktrees/") || p.contains("/.git/modules/") {
                    return false;
                }
                let target = if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    dir.join(p)
                };
                return target.exists();
            }
        }
    }
    false
}

/// Find repos under `root`: BFS for directories containing a valid `.git`
/// entry (dir or file — worktrees/submodules use a file). Once a repo is found
/// we don't descend into it, so submodules fold into their parent and the
/// mono-repo case degrades to a single repo at `""` (the root itself).
pub fn discover(root: &Path) -> Vec<String> {
    if is_repo_dir(root) {
        return vec![String::new()];
    }
    let mut found: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    let mut queue: VecDeque<(PathBuf, String, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new(), 0));
    while let Some((dir, prefix, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH || scanned >= MAX_SCAN_DIRS {
            continue;
        }
        scanned += 1;
        let Ok(read_dir) = std::fs::read_dir(&dir) else { continue };
        let mut children: Vec<std::fs::DirEntry> = read_dir.flatten().collect();
        children.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
        for entry in children {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Hidden dirs (.worktrees, .cache, .venv, …) hold tooling
            // checkouts and caches, not project repos — never descend.
            if name.starts_with('.') || EXCLUDED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            if is_repo_dir(&entry.path()) {
                found.push(rel);
            } else {
                queue.push_back((entry.path(), rel, depth + 1));
            }
        }
    }
    found
}

// ---- running git ---------------------------------------------------------

/// Run `<program> <args>` in `dir`; errors carry stderr.
async fn run_prog(program: &str, dir: &Path, args: &[&str], secs: u64) -> Result<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    // Windows: without CREATE_NO_WINDOW every `git` call flashes its own console
    // window. Git runs constantly (per-repo status/log on load, and update.rs's
    // poller fires a burst of ~7 commands at startup and every 30 min), so the
    // app opens a storm of consoles without this.
    crate::agents::no_console(&mut cmd);
    let out = tokio::time::timeout(std::time::Duration::from_secs(secs), cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("{program} {} timed out", args.first().unwrap_or(&"")))?
        .with_context(|| format!("failed to run {program} — is it installed?"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = if stderr.trim().is_empty() { stdout } else { stderr.into_owned() };
        anyhow::bail!("{}", msg.trim())
    }
}

/// Run `git <args>` in `repo`. Network-facing commands (push/pull/fetch) get a
/// longer leash than local ones.
pub(crate) async fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let network = matches!(args.first().copied(), Some("push" | "pull" | "fetch"));
    run_prog("git", repo, args, if network { 120 } else { 30 }).await
}

/// Reject branch names git would misparse (leading '-') or refuse — a cheap
/// stand-in for `git check-ref-format --branch` that can't be called safely
/// with hostile input as its own argument.
fn validate_branch(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name.starts_with('-')
        || name.starts_with('.')
        || name.ends_with('/')
        || name.ends_with(".lock")
        || name.contains("..")
        || name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\'));
    anyhow::ensure!(!bad, "invalid branch name: {name}");
    Ok(())
}

// ---- status (porcelain v2) ----------------------------------------------

#[derive(Default)]
struct RepoStatus {
    branch: String,
    detached: bool,
    upstream: Option<String>,
    ahead: i64,
    behind: i64,
    entries: Vec<Value>, // { path, origPath?, x, y, kind }
    staged: usize,
    unstaged: usize,
    untracked: usize,
    conflicted: usize,
}

/// Parse `git status --porcelain=v2 --branch -z` output. `-z` NUL-terminates
/// records; a rename record ("2 …") is followed by one extra NUL-separated
/// field holding the original path.
fn parse_status(raw: &str) -> RepoStatus {
    let mut st = RepoStatus::default();
    let mut records = raw.split('\0');
    while let Some(rec) = records.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(header) = rec.strip_prefix("# ") {
            let mut it = header.splitn(2, ' ');
            match (it.next(), it.next()) {
                (Some("branch.head"), Some(v)) => {
                    st.detached = v == "(detached)";
                    st.branch = if st.detached { "detached HEAD".into() } else { v.into() };
                }
                (Some("branch.upstream"), Some(v)) => st.upstream = Some(v.into()),
                (Some("branch.ab"), Some(v)) => {
                    for part in v.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            st.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            st.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind_char = rec.chars().next().unwrap_or(' ');
        match kind_char {
            '1' | '2' => {
                let fields: Vec<&str> = rec.splitn(if kind_char == '1' { 9 } else { 10 }, ' ').collect();
                let (Some(xy), Some(path)) = (fields.get(1), fields.last()) else { continue };
                let mut chars = xy.chars();
                let x = chars.next().unwrap_or('.');
                let y = chars.next().unwrap_or('.');
                let orig = if kind_char == '2' { records.next() } else { None };
                if x != '.' {
                    st.staged += 1;
                }
                if y != '.' {
                    st.unstaged += 1;
                }
                let mut entry = json!({
                    "path": path,
                    "x": x.to_string(),
                    "y": y.to_string(),
                    "kind": "changed",
                });
                if let Some(orig) = orig {
                    entry["origPath"] = json!(orig);
                }
                st.entries.push(entry);
            }
            'u' => {
                let fields: Vec<&str> = rec.splitn(11, ' ').collect();
                if let Some(path) = fields.last() {
                    st.conflicted += 1;
                    st.entries.push(json!({
                        "path": path,
                        "x": "U", "y": "U",
                        "kind": "conflicted",
                    }));
                }
            }
            '?' => {
                if let Some(path) = rec.get(2..) {
                    st.untracked += 1;
                    st.entries.push(json!({
                        "path": path,
                        "x": "?", "y": "?",
                        "kind": "untracked",
                    }));
                }
            }
            _ => {}
        }
    }
    st
}

async fn status(repo: &Path) -> Result<RepoStatus> {
    let raw = run_git(repo, &["status", "--porcelain=v2", "--branch", "-z"]).await?;
    Ok(parse_status(&raw))
}

fn status_json(repo_id: &str, st: &RepoStatus) -> Value {
    json!({
        "repoId": repo_id,
        "branch": st.branch,
        "detached": st.detached,
        "upstream": st.upstream,
        "ahead": st.ahead,
        "behind": st.behind,
        "staged": st.staged,
        "unstaged": st.unstaged,
        "untracked": st.untracked,
        "conflicted": st.conflicted,
        "entries": st.entries,
    })
}

/// Fleet-overview summary for one repo: status counts + last commit.
async fn summary(record: &RepoRecord, project_name: &str, abs: &Path) -> Value {
    let name = if record.rel_path.is_empty() {
        project_name.to_string()
    } else {
        record
            .rel_path
            .rsplit('/')
            .next()
            .unwrap_or(&record.rel_path)
            .to_string()
    };
    let mut out = json!({
        "id": record.id,
        "projectId": record.project_id,
        "relPath": record.rel_path,
        "name": name,
    });
    match status(abs).await {
        Ok(st) => {
            out["branch"] = json!(st.branch);
            out["detached"] = json!(st.detached);
            out["upstream"] = json!(st.upstream);
            out["ahead"] = json!(st.ahead);
            out["behind"] = json!(st.behind);
            out["staged"] = json!(st.staged);
            out["unstaged"] = json!(st.unstaged);
            out["untracked"] = json!(st.untracked);
            out["conflicted"] = json!(st.conflicted);
        }
        Err(e) => {
            out["error"] = json!(format!("{e:#}"));
            return out;
        }
    }
    // %x1f = unit separator — subjects can contain anything printable.
    if let Ok(line) = run_git(abs, &["log", "-1", "--format=%h\u{1f}%s\u{1f}%cI"]).await {
        let parts: Vec<&str> = line.trim_end().split('\u{1f}').collect();
        if parts.len() == 3 {
            out["lastCommit"] = json!({
                "hash": parts[0],
                "subject": parts[1],
                "at": parts[2],
            });
        }
    }
    out
}

// ---- history -------------------------------------------------------------

/// Commits per `git.log` page when the client doesn't say, and the ceiling.
const LOG_PAGE: usize = 200;
const LOG_PAGE_MAX: usize = 1000;

/// One `git.log` record: NUL-separated via `-z`, fields split on %x1f. Full
/// refnames (`--decorate=full`) are what let `parse_refs` tell a remote branch
/// from a local one that happens to contain a slash.
const LOG_FORMAT: &str = "%H\u{1f}%h\u{1f}%P\u{1f}%an\u{1f}%ae\u{1f}%aI\u{1f}%D\u{1f}%s";

/// A commit hash is the one user value that reaches git as a bare revision
/// argument, so it is held to hex before git sees it: no leading '-', no
/// revision expressions.
fn validate_hash(hash: &str) -> Result<()> {
    anyhow::ensure!(
        (4..=64).contains(&hash.len()) && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid commit hash: {hash}"
    );
    Ok(())
}

/// Same guard `paths_field` applies, for a single optional path.
fn validate_rel_path(p: &str) -> Result<()> {
    anyhow::ensure!(
        !p.is_empty() && !p.starts_with('/') && !p.starts_with('-') && !p.split('/').any(|c| c == ".."),
        "bad path: {p}"
    );
    Ok(())
}

/// `%D` under `--decorate=full` → ref chips. "HEAD -> x" marks the checked-out
/// branch; a bare "HEAD" is a detached checkout. Stash, notes and replace refs
/// are dropped, as is every remote's `HEAD` pointer.
fn parse_refs(decorations: &str) -> Vec<Value> {
    let mut refs = Vec::new();
    for item in decorations.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (current, name) = match item.strip_prefix("HEAD -> ") {
            Some(rest) => (true, rest),
            None => (false, item),
        };
        let (kind, short) = if let Some(b) = name.strip_prefix("refs/heads/") {
            ("local", b)
        } else if let Some(r) = name.strip_prefix("refs/remotes/") {
            if r.ends_with("/HEAD") {
                continue;
            }
            ("remote", r)
        } else if let Some(t) = name.strip_prefix("tag: refs/tags/") {
            ("tag", t)
        } else if name == "HEAD" {
            ("head", "HEAD")
        } else {
            continue;
        };
        let mut r = json!({ "name": short, "kind": kind });
        if current || kind == "head" {
            r["current"] = json!(true);
        }
        refs.push(r);
    }
    refs
}

fn parse_log(raw: &str) -> Vec<Value> {
    raw.split('\0')
        .filter(|rec| !rec.is_empty())
        .filter_map(|rec| {
            let f: Vec<&str> = rec.splitn(8, '\u{1f}').collect();
            if f.len() < 8 {
                return None;
            }
            Some(json!({
                "hash": f[0],
                "short": f[1],
                "parents": f[2].split_whitespace().collect::<Vec<_>>(),
                "author": f[3],
                "authorEmail": f[4],
                "at": f[5],
                "refs": parse_refs(f[6]),
                "subject": f[7],
            }))
        })
        .collect()
}

/// `diff-tree --name-status -z`: `<status>\0<path>\0`; renames and copies
/// (`R<score>`, `C<score>`) carry `<old>\0<new>\0` instead.
fn parse_name_status(raw: &str) -> Vec<Value> {
    let mut files = Vec::new();
    let mut it = raw.split('\0');
    while let Some(status) = it.next() {
        if status.is_empty() {
            continue;
        }
        let letter = status.chars().next().unwrap_or('M').to_string();
        let Some(first) = it.next() else { break };
        if status.starts_with('R') || status.starts_with('C') {
            let Some(new) = it.next() else { break };
            files.push(json!({ "path": new, "origPath": first, "status": letter }));
        } else {
            files.push(json!({ "path": first, "status": letter }));
        }
    }
    files
}

/// `diff-tree --numstat -z`: `<adds>\t<dels>\t<path>\0`; renames and copies
/// carry `<adds>\t<dels>\t\0<old>\0<new>\0`; binaries have `-` for both
/// counts. Keyed by the new path; the value is (adds, dels, binary).
fn parse_numstat(raw: &str) -> std::collections::HashMap<String, (i64, i64, bool)> {
    let mut out = std::collections::HashMap::new();
    let mut it = raw.split('\0');
    while let Some(rec) = it.next() {
        if rec.is_empty() {
            continue;
        }
        let mut parts = rec.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if path.is_empty() {
            let _old = it.next();
            match it.next() {
                Some(new) => new,
                None => break,
            }
        } else {
            path
        };
        out.insert(
            path.to_string(),
            (a.parse().unwrap_or(0), d.parse().unwrap_or(0), a == "-"),
        );
    }
    out
}

/// Files a commit touched, against its first parent (a merge shows what the
/// merge did to the branch it landed on) or the empty tree for a root commit.
async fn commit_files(repo: &Path, hash: &str, first_parent: Option<&str>) -> Result<Vec<Value>> {
    let base = ["diff-tree", "--no-commit-id", "-r", "-M", "-z"];
    let mut names: Vec<&str> = base.to_vec();
    let mut counts: Vec<&str> = base.to_vec();
    names.push("--name-status");
    counts.push("--numstat");
    match first_parent {
        Some(p) => {
            names.extend([p, hash]);
            counts.extend([p, hash]);
        }
        None => {
            names.extend(["--root", hash]);
            counts.extend(["--root", hash]);
        }
    }
    let (names_out, counts_out) = tokio::try_join!(run_git(repo, &names), run_git(repo, &counts))?;
    let stats = parse_numstat(&counts_out);
    let mut files = parse_name_status(&names_out);
    for f in &mut files {
        let Some((adds, dels, binary)) = f["path"].as_str().and_then(|p| stats.get(p)) else {
            continue;
        };
        if *binary {
            f["binary"] = json!(true);
        } else {
            f["additions"] = json!(adds);
            f["deletions"] = json!(dels);
        }
    }
    Ok(files)
}

/// A branch name from the UI → the one ref to walk. Local heads win; a
/// remote-only name (`feature`) or a decorated one (`origin/feature`) both
/// resolve to the remote ref. The UI never sees a remote's `HEAD` pointer.
async fn resolve_branch(repo: &Path, name: &str) -> Result<String> {
    validate_branch(name)?;
    let local = format!("refs/heads/{name}");
    let remote = format!("refs/remotes/{name}");
    let any_remote = format!("refs/remotes/*/{name}");
    let raw = run_git(
        repo,
        &["for-each-ref", "--format=%(refname)", &local, &remote, &any_remote],
    )
    .await?;
    raw.lines()
        .find(|l| *l == local || *l == remote || (l.starts_with("refs/remotes/") && l.ends_with(&format!("/{name}"))))
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("unknown branch: {name}"))
}

/// Cap and classify a unified diff the way every diff request answers.
fn diff_json(path: &str, unified: String) -> Value {
    let binary = unified.contains("Binary files ") && unified.lines().count() <= 5;
    let truncated = unified.len() > DIFF_CAP;
    let unified = if truncated {
        let mut end = DIFF_CAP;
        while !unified.is_char_boundary(end) {
            end -= 1;
        }
        unified[..end].to_string()
    } else {
        unified
    };
    json!({ "path": path, "unified": unified, "truncated": truncated, "binary": binary })
}

// ---- request handling ----------------------------------------------------

fn field<'a>(payload: &'a Value, key: &str) -> Result<&'a str> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing field: {key}"))
}

fn paths_field(payload: &Value) -> Result<Vec<String>> {
    let paths: Vec<String> = payload
        .get("paths")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| anyhow::anyhow!("bad paths"))?
        .unwrap_or_default();
    anyhow::ensure!(!paths.is_empty(), "no paths given");
    // Git itself rejects pathspecs that escape the repo; this guards the one
    // operation (untracked discard) that touches the filesystem directly and
    // keeps obviously hostile input out of every git invocation.
    for p in &paths {
        anyhow::ensure!(
            !p.is_empty() && !p.starts_with('/') && !p.split('/').any(|c| c == ".."),
            "bad path: {p}"
        );
    }
    Ok(paths)
}

/// Resolve a repoId to its record + project + absolute path on disk.
fn resolve(state: &ServerState, repo_id: &str) -> Result<(RepoRecord, crate::protocol::Project, PathBuf)> {
    let record = state
        .hub
        .store
        .repo(repo_id)
        .ok_or_else(|| anyhow::anyhow!("unknown repo"))?;
    let project = state
        .hub
        .store
        .project(&record.project_id)
        .ok_or_else(|| anyhow::anyhow!("unknown project"))?;
    let abs = if record.rel_path.is_empty() {
        PathBuf::from(&project.path)
    } else {
        Path::new(&project.path).join(&record.rel_path)
    };
    anyhow::ensure!(
        is_repo_dir(&abs),
        "not a git repository (moved or corrupt?): {}",
        abs.display()
    );
    Ok((record, project, abs))
}

/// Dispatch all `git.*` requests (see docs/PROTOCOL.md).
pub async fn handle(state: &ServerState, kind: &str, payload: &Value) -> Result<Value> {
    match kind {
        // Scan the project folder, reconcile with persisted records (ids are
        // stable, keyed by relPath) and return live per-repo summaries.
        "git.repos" => {
            let project_id = field(payload, "projectId")?;
            let project = state
                .hub
                .store
                .project(project_id)
                .ok_or_else(|| anyhow::anyhow!("unknown project"))?;
            let root = PathBuf::from(&project.path);
            let rel_paths = tokio::task::spawn_blocking(move || discover(&root)).await?;
            let records = state.hub.store.reconcile_repos(project_id, &rel_paths)?;
            let futures = records.iter().map(|r| {
                let abs = if r.rel_path.is_empty() {
                    PathBuf::from(&project.path)
                } else {
                    Path::new(&project.path).join(&r.rel_path)
                };
                let name = project.name.clone();
                async move { summary(r, &name, &abs).await }
            });
            let repos: Vec<Value> = futures_util::future::join_all(futures).await;
            Ok(json!({ "repos": repos }))
        }
        "git.status" => {
            let (record, _, abs) = resolve(state, field(payload, "repoId")?)?;
            let st = status(&abs).await?;
            Ok(status_json(&record.id, &st))
        }
        "git.diff" => {
            let (_, _, abs) = resolve(state, field(payload, "repoId")?)?;
            let path = field(payload, "path")?.to_string();
            let scope = payload.get("scope").and_then(|v| v.as_str()).unwrap_or("worktree");
            let unified = match scope {
                "untracked" => untracked_diff(&abs, &path)?,
                "staged" => run_git(&abs, &["diff", "--cached", "--", &path]).await?,
                _ => run_git(&abs, &["diff", "--", &path]).await?,
            };
            Ok(diff_json(&path, unified))
        }
        "git.stage" => {
            let (record, project, abs) = resolve(state, field(payload, "repoId")?)?;
            let paths = paths_field(payload)?;
            let mut args = vec!["add", "--"];
            args.extend(paths.iter().map(String::as_str));
            run_git(&abs, &args).await?;
            after_mutation(state, &record.id, &project.id, &abs).await
        }
        "git.unstage" => {
            let (record, project, abs) = resolve(state, field(payload, "repoId")?)?;
            let paths = paths_field(payload)?;
            let mut args = vec!["restore", "--staged", "--"];
            args.extend(paths.iter().map(String::as_str));
            run_git(&abs, &args).await?;
            after_mutation(state, &record.id, &project.id, &abs).await
        }
        // Destructive: throw away worktree changes (tracked) or delete the
        // file (untracked). The UI confirms before calling this.
        "git.discard" => {
            let (record, project, abs) = resolve(state, field(payload, "repoId")?)?;
            let paths = paths_field(payload)?;
            let st = status(&abs).await?;
            let untracked: std::collections::HashSet<&str> = st
                .entries
                .iter()
                .filter(|e| e["kind"] == "untracked")
                .filter_map(|e| e["path"].as_str())
                .collect();
            let (remove, restore): (Vec<&String>, Vec<&String>) =
                paths.iter().partition(|p| untracked.contains(p.as_str()));
            if !restore.is_empty() {
                let mut args = vec!["restore", "--"];
                args.extend(restore.iter().map(|s| s.as_str()));
                run_git(&abs, &args).await?;
            }
            for p in remove {
                let target = crate::files::confine(&abs, p)?;
                std::fs::remove_file(&target)
                    .with_context(|| format!("delete {}", target.display()))?;
            }
            after_mutation(state, &record.id, &project.id, &abs).await
        }
        "git.commit" => {
            let (record, project, abs) = resolve(state, field(payload, "repoId")?)?;
            let message = field(payload, "message")?.trim().to_string();
            anyhow::ensure!(!message.is_empty(), "commit message is empty");
            run_git(&abs, &["commit", "-m", &message]).await?;
            let line = run_git(&abs, &["log", "-1", "--format=%h\u{1f}%s"]).await?;
            let mut parts = line.trim_end().split('\u{1f}');
            let (hash, subject) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let mut out = after_mutation(state, &record.id, &project.id, &abs).await?;
            out["hash"] = json!(hash);
            out["subject"] = json!(subject);
            Ok(out)
        }
        "git.branches" => {
            let (_, _, abs) = resolve(state, field(payload, "repoId")?)?;
            // Local heads AND remote branches — most branches usually live
            // only on origin. Checking out a remote-only name DWIMs into a
            // local tracking branch, so the client can offer them directly.
            let raw = run_git(
                &abs,
                &["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"],
            )
            .await?;
            let mut local: Vec<String> = Vec::new();
            let mut remote: Vec<String> = Vec::new();
            for line in raw.lines() {
                if let Some(name) = line.strip_prefix("refs/heads/") {
                    local.push(name.to_string());
                } else if let Some(rest) = line.strip_prefix("refs/remotes/") {
                    // "<remote>/<branch>" — drop the remote segment; skip the
                    // symbolic HEAD entry.
                    if let Some((_, branch)) = rest.split_once('/') {
                        if branch != "HEAD" && !remote.contains(&branch.to_string()) {
                            remote.push(branch.to_string());
                        }
                    }
                }
            }
            remote.retain(|b| !local.contains(b));
            let st = status(&abs).await?;
            Ok(json!({
                "current": st.branch,
                "detached": st.detached,
                "branches": local,
                "remoteBranches": remote,
            }))
        }
        "git.checkout" => {
            let (record, project, abs) = resolve(state, field(payload, "repoId")?)?;
            let branch = field(payload, "branch")?;
            validate_branch(branch)?;
            let create = payload.get("create").and_then(|v| v.as_bool()).unwrap_or(false);
            if create {
                run_git(&abs, &["checkout", "-b", branch]).await?;
            } else {
                run_git(&abs, &["checkout", branch]).await?;
            }
            after_mutation(state, &record.id, &project.id, &abs).await
        }
        // One action, several repos: stage-all (optional) + commit each entry.
        // With `link` and ≥2 entries every message gets a shared
        // `Threadknot-Change: <id>` trailer so related commits across repos stay
        // discoverable later. Per-repo failures don't stop the rest.
        "git.commitMany" => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Entry {
                repo_id: String,
                message: String,
                #[serde(default)]
                stage_all: bool,
            }
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct CommitMany {
                project_id: String,
                entries: Vec<Entry>,
                #[serde(default)]
                link: bool,
            }
            let req: CommitMany = serde_json::from_value(payload.clone())?;
            anyhow::ensure!(!req.entries.is_empty(), "no repos selected");
            let change_id = (req.link && req.entries.len() >= 2)
                .then(|| crate::protocol::new_id()[..12].to_string());
            let mut results: Vec<Value> = Vec::new();
            for entry in &req.entries {
                let outcome = async {
                    let (record, _, abs) = resolve(state, &entry.repo_id)?;
                    anyhow::ensure!(
                        record.project_id == req.project_id,
                        "repo belongs to another project"
                    );
                    let message = entry.message.trim();
                    anyhow::ensure!(!message.is_empty(), "commit message is empty");
                    if entry.stage_all {
                        run_git(&abs, &["add", "-A"]).await?;
                    }
                    let full = match &change_id {
                        Some(id) => format!("{message}\n\nThreadknot-Change: {id}"),
                        None => message.to_string(),
                    };
                    run_git(&abs, &["commit", "-m", &full]).await?;
                    let line = run_git(&abs, &["log", "-1", "--format=%h\u{1f}%s"]).await?;
                    let mut parts = line.trim_end().split('\u{1f}');
                    Ok::<(String, String), anyhow::Error>((
                        parts.next().unwrap_or("").to_string(),
                        parts.next().unwrap_or("").to_string(),
                    ))
                }
                .await;
                results.push(match outcome {
                    Ok((hash, subject)) => json!({
                        "repoId": entry.repo_id, "ok": true, "hash": hash, "subject": subject,
                    }),
                    Err(e) => json!({
                        "repoId": entry.repo_id, "ok": false, "error": format!("{e:#}"),
                    }),
                });
            }
            state
                .hub
                .broadcast_state("git", Some(req.project_id.clone()));
            Ok(json!({ "results": results, "changeId": change_id }))
        }
        // Same branch across several repos: switch where it exists, create
        // where it doesn't. Per-repo failures don't stop the rest.
        "git.checkoutMany" => {
            let project_id = field(payload, "projectId")?.to_string();
            let branch = field(payload, "branch")?.to_string();
            validate_branch(&branch)?;
            let repo_ids: Vec<String> = payload
                .get("repoIds")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| anyhow::anyhow!("bad repoIds"))?
                .unwrap_or_default();
            anyhow::ensure!(!repo_ids.is_empty(), "no repos selected");
            let mut results: Vec<Value> = Vec::new();
            for repo_id in &repo_ids {
                let outcome = async {
                    let (record, _, abs) = resolve(state, repo_id)?;
                    anyhow::ensure!(
                        record.project_id == project_id,
                        "repo belongs to another project"
                    );
                    let refname = format!("refs/heads/{branch}");
                    let exists = run_git(&abs, &["rev-parse", "--verify", "--quiet", &refname])
                        .await
                        .is_ok();
                    if exists {
                        run_git(&abs, &["checkout", &branch]).await?;
                    } else {
                        run_git(&abs, &["checkout", "-b", &branch]).await?;
                    }
                    Ok::<bool, anyhow::Error>(!exists)
                }
                .await;
                results.push(match outcome {
                    Ok(created) => json!({ "repoId": repo_id, "ok": true, "created": created }),
                    Err(e) => json!({ "repoId": repo_id, "ok": false, "error": format!("{e:#}") }),
                });
            }
            state.hub.broadcast_state("git", Some(project_id));
            Ok(json!({ "results": results }))
        }
        // Open a pull request via the installed `gh` CLI (inherits its auth).
        "git.pr" => {
            let (_, _, abs) = resolve(state, field(payload, "repoId")?)?;
            let title = payload.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let output = if title.is_empty() {
                // --fill uses the commit(s) on the branch for title/body.
                run_prog("gh", &abs, &["pr", "create", "--fill"], 60).await?
            } else {
                run_prog("gh", &abs, &["pr", "create", "--title", title, "--body", body], 60)
                    .await?
            };
            let url = output
                .lines()
                .rev()
                .find(|l| l.trim_start().starts_with("https://"))
                .map(|l| l.trim().to_string());
            Ok(json!({ "url": url, "output": output.trim() }))
        }
        "git.push" => {
            let (record, project, abs) = resolve(state, field(payload, "repoId")?)?;
            let output = match run_git(&abs, &["push"]).await {
                Ok(o) => o,
                // First push of a new branch: set the upstream and retry.
                Err(e) if format!("{e:#}").contains("no upstream") => {
                    run_git(&abs, &["push", "-u", "origin", "HEAD"]).await?
                }
                Err(e) => return Err(e),
            };
            let mut out = after_mutation(state, &record.id, &project.id, &abs).await?;
            out["output"] = json!(output.trim());
            Ok(out)
        }
        "git.pull" => {
            let (record, project, abs) = resolve(state, field(payload, "repoId")?)?;
            let output = run_git(&abs, &["pull", "--ff-only"]).await?;
            let mut out = after_mutation(state, &record.id, &project.id, &abs).await?;
            out["output"] = json!(output.trim());
            Ok(out)
        }
        // One page of history, newest first. `--date-order` guarantees no
        // parent precedes its children, which is what lets the client lay the
        // branch graph out in a single pass over the page.
        "git.log" => {
            let (_, _, abs) = resolve(state, field(payload, "repoId")?)?;
            let limit = payload
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(LOG_PAGE)
                .clamp(1, LOG_PAGE_MAX);
            let skip = payload.get("skip").and_then(|v| v.as_u64()).unwrap_or(0);
            let opt = |key: &str| {
                payload
                    .get(key)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let (branch, query, author, path) = (opt("branch"), opt("query"), opt("author"), opt("path"));
            if let Some(p) = &path {
                validate_rel_path(p)?;
            }
            let revision = match &branch {
                Some(b) => Some(resolve_branch(&abs, b).await?),
                None => None,
            };
            // A hex query that names a commit is a jump, not a search.
            let mut pinned: Option<String> = None;
            if let Some(q) = query.as_deref().filter(|q| validate_hash(q).is_ok()) {
                let spec = format!("{q}^{{commit}}");
                if let Ok(full) = run_git(&abs, &["rev-parse", "--verify", "--quiet", &spec]).await {
                    pinned = Some(full.trim().to_string());
                }
            }
            let mut args: Vec<String> = vec![
                "log".into(),
                "--date-order".into(),
                "--decorate=full".into(),
                "-z".into(),
                format!("--format={LOG_FORMAT}"),
                format!("--max-count={}", limit + 1),
                format!("--skip={skip}"),
                // -F makes --grep and --author literal matches.
                "-F".into(),
                "-i".into(),
            ];
            if let Some(a) = &author {
                args.push(format!("--author={a}"));
            }
            match (&pinned, &revision) {
                // The one commit the hash names — not its ancestry.
                (Some(h), _) => args.extend(["--max-count=1".into(), "--skip=0".into(), h.clone()]),
                (None, Some(r)) => {
                    if let Some(q) = &query {
                        args.push(format!("--grep={q}"));
                    }
                    args.push(r.clone());
                }
                (None, None) => {
                    if let Some(q) = &query {
                        args.push(format!("--grep={q}"));
                    }
                    // The refs the chips can name — not stash, notes, or
                    // tooling refs like refs/t3/… that --all would drag in.
                    args.extend(["--branches".into(), "--remotes".into(), "--tags".into(), "HEAD".into()]);
                }
            }
            args.push("--".into());
            if let Some(p) = &path {
                args.push(p.clone());
            }
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let raw = match run_git(&abs, &argv).await {
                Ok(raw) => raw,
                // An unborn branch has no history, not an error.
                Err(e) if format!("{e:#}").contains("does not have any commits") => String::new(),
                Err(e) => return Err(e),
            };
            let mut commits = parse_log(&raw);
            let has_more = commits.len() > limit;
            commits.truncate(limit);
            let head = run_git(&abs, &["rev-parse", "--verify", "--quiet", "HEAD"])
                .await
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            Ok(json!({ "commits": commits, "hasMore": has_more, "head": head }))
        }
        "git.show" => {
            let (_, _, abs) = resolve(state, field(payload, "repoId")?)?;
            let hash = field(payload, "hash")?;
            validate_hash(hash)?;
            const FMT: &str = "--format=%H\u{1f}%h\u{1f}%P\u{1f}%an\u{1f}%ae\u{1f}%aI\u{1f}%cn\u{1f}%cI\u{1f}%D\u{1f}%s\u{1f}%b";
            let raw = run_git(&abs, &["show", "-s", "--decorate=full", FMT, hash, "--"]).await?;
            let f: Vec<&str> = raw.splitn(11, '\u{1f}').collect();
            anyhow::ensure!(f.len() == 11, "unexpected `git show` output");
            let parents: Vec<&str> = f[2].split_whitespace().collect();
            let files = commit_files(&abs, f[0], parents.first().copied()).await?;
            Ok(json!({
                "hash": f[0],
                "short": f[1],
                "parents": parents,
                "author": f[3],
                "authorEmail": f[4],
                "at": f[5],
                "committer": f[6],
                "committedAt": f[7],
                "refs": parse_refs(f[8]),
                "subject": f[9],
                "body": f[10].trim(),
                "files": files,
            }))
        }
        // One file's patch inside a commit. A merge is shown against its first
        // parent — the combined format is not what anyone reviewing wants.
        "git.commitDiff" => {
            let (_, _, abs) = resolve(state, field(payload, "repoId")?)?;
            let hash = field(payload, "hash")?;
            validate_hash(hash)?;
            let path = field(payload, "path")?.to_string();
            validate_rel_path(&path)?;
            let orig = payload.get("origPath").and_then(|v| v.as_str()).map(str::to_string);
            if let Some(o) = &orig {
                validate_rel_path(o)?;
            }
            let line = run_git(&abs, &["rev-list", "--parents", "-n", "1", hash, "--"]).await?;
            let mut ids = line.split_whitespace();
            let full = ids.next().unwrap_or(hash).to_string();
            let parents: Vec<&str> = ids.collect();
            let mut args: Vec<&str> = if parents.len() > 1 {
                vec!["diff", "-M", parents[0], &full]
            } else {
                vec!["show", "--format=", "-M", &full]
            };
            args.push("--");
            args.push(&path);
            if let Some(o) = &orig {
                args.push(o);
            }
            let unified = run_git(&abs, &args).await?;
            Ok(diff_json(&path, unified))
        }
        other => anyhow::bail!("unknown request type: {other}"),
    }
}

/// Every mutation returns the fresh status (saves the client a round trip)
/// and broadcasts so other clients' fleet views refresh.
async fn after_mutation(
    state: &ServerState,
    repo_id: &str,
    project_id: &str,
    abs: &Path,
) -> Result<Value> {
    let st = status(abs).await?;
    state.hub.broadcast_state("git", Some(project_id.to_string()));
    Ok(status_json(repo_id, &st))
}

/// Synthesized all-added diff for an untracked file (git diff doesn't cover
/// untracked paths without --no-index exit-code quirks).
fn untracked_diff(repo: &Path, rel: &str) -> Result<String> {
    let target = crate::files::confine(repo, rel)?;
    use std::io::Read;
    let mut window = Vec::new();
    std::fs::File::open(&target)?
        .take(DIFF_CAP as u64)
        .read_to_end(&mut window)?;
    if window.contains(&0) {
        return Ok(format!("Binary files /dev/null and b/{rel} differ\n"));
    }
    let text = String::from_utf8_lossy(&window);
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!("--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n", lines.len());
    for line in lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2() {
        let raw = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
                   1 .M N... 100644 100644 100644 abc def src/app.ts\0\
                   1 A. N... 000000 100644 100644 000 def new.ts\0\
                   2 R. N... 100644 100644 100644 abc def R100 renamed.ts\0old.ts\0\
                   ? scratch.txt\0";
        let st = parse_status(raw);
        assert_eq!(st.branch, "main");
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!(st.ahead, 2);
        assert_eq!(st.behind, 1);
        assert_eq!(st.entries.len(), 4);
        assert_eq!(st.staged, 2); // A. and R.
        assert_eq!(st.unstaged, 1); // .M
        assert_eq!(st.untracked, 1);
        assert_eq!(st.entries[2]["origPath"], "old.ts");
        assert_eq!(st.entries[3]["kind"], "untracked");
    }

    #[test]
    fn rejects_invalid_dotgit() {
        let base = std::env::temp_dir().join(format!("threadknot-gittest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        // Empty .git dir (the "01 Service Storm Full" case) → not a repo.
        let empty = base.join("empty");
        std::fs::create_dir_all(empty.join(".git")).unwrap();
        assert!(!is_repo_dir(&empty));

        // .git dir with HEAD → a repo.
        let real = base.join("real");
        std::fs::create_dir_all(real.join(".git")).unwrap();
        std::fs::write(real.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(is_repo_dir(&real));

        // .git file with a dangling gitdir pointer → not a repo.
        let dangling = base.join("dangling");
        std::fs::create_dir_all(&dangling).unwrap();
        std::fs::write(dangling.join(".git"), "gitdir: /nonexistent/worktrees/x\n").unwrap();
        assert!(!is_repo_dir(&dangling));

        // .git file with junk contents → not a repo.
        let junk = base.join("junk");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join(".git"), "hello\n").unwrap();
        assert!(!is_repo_dir(&junk));

        // Linked worktree / submodule pointers → checkouts, not repos.
        let wt = base.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}/.git/worktrees/wt\n", real.display()),
        )
        .unwrap();
        assert!(!is_repo_dir(&wt));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join(".git"),
            format!("gitdir: {}/.git/modules/sub\n", real.display()),
        )
        .unwrap();
        assert!(!is_repo_dir(&sub));

        // Hidden dirs are never descended into (worktree conventions live there).
        let hidden = base.join(".worktrees").join("feature-x");
        std::fs::create_dir_all(hidden.join(".git")).unwrap();
        std::fs::write(hidden.join(".git/HEAD"), "ref: refs/heads/x\n").unwrap();

        // Discovery skips the invalid ones but still descends into them?
        // No — invalid `.git` folders are treated as plain dirs, so a valid
        // repo nested beneath one is still found.
        let nested = empty.join("inner");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::write(nested.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let mut found = discover(&base);
        found.sort();
        assert_eq!(found, vec!["empty/inner".to_string(), "real".to_string()]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_detached_and_conflicts() {
        let raw = "# branch.oid abc\0# branch.head (detached)\0\
                   u UU N... 100644 100644 100644 100644 a b c both.ts\0";
        let st = parse_status(raw);
        assert!(st.detached);
        assert_eq!(st.conflicted, 1);
        assert_eq!(st.entries[0]["kind"], "conflicted");
    }

    #[test]
    fn parses_full_decorations() {
        let refs = parse_refs(
            "HEAD -> refs/heads/master, refs/remotes/origin/master, refs/remotes/origin/HEAD, \
             tag: refs/tags/v0.2.1, refs/heads/feat/x, refs/stash",
        );
        let names: Vec<(&str, &str, bool)> = refs
            .iter()
            .map(|r| {
                (
                    r["name"].as_str().unwrap(),
                    r["kind"].as_str().unwrap(),
                    r["current"].as_bool().unwrap_or(false),
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                ("master", "local", true),
                ("origin/master", "remote", false),
                ("v0.2.1", "tag", false),
                ("feat/x", "local", false),
            ]
        );
        let detached = parse_refs("HEAD, refs/heads/master");
        assert_eq!(detached[0]["kind"], "head");
        assert_eq!(detached[0]["current"], true);
        assert_eq!(detached[1]["current"], Value::Null);
        assert!(parse_refs("").is_empty());
    }

    #[test]
    fn parses_log_records() {
        let raw = "aaaa\u{1f}aaa\u{1f}bbbb cccc\u{1f}spencer\u{1f}s@x\u{1f}2026-09-07T12:30:29-04:00\u{1f}HEAD -> refs/heads/master\u{1f}Merge it\0\
                   bbbb\u{1f}bbb\u{1f}\u{1f}Oscar\u{1f}o@x\u{1f}2026-09-02T15:57:29-04:00\u{1f}\u{1f}Root commit\0";
        let commits = parse_log(raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0]["parents"], json!(["bbbb", "cccc"]));
        assert_eq!(commits[0]["refs"][0]["name"], "master");
        assert_eq!(commits[0]["subject"], "Merge it");
        assert_eq!(commits[1]["parents"], json!([]));
        assert_eq!(commits[1]["refs"], json!([]));
        assert_eq!(commits[1]["subject"], "Root commit");
    }

    #[test]
    fn parses_diff_tree_z() {
        let names = "M\0src/app.ts\0R100\0old.ts\0new.ts\0A\0bin.dat\0D\0gone.md\0";
        let files = parse_name_status(names);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0], json!({ "path": "src/app.ts", "status": "M" }));
        assert_eq!(files[1], json!({ "path": "new.ts", "origPath": "old.ts", "status": "R" }));
        assert_eq!(files[3]["status"], "D");

        let counts = "3\t1\tsrc/app.ts\0-\t-\tbin.dat\00\t0\t\0old.ts\0new.ts\00\t12\tgone.md\0";
        let stats = parse_numstat(counts);
        assert_eq!(stats["src/app.ts"], (3, 1, false));
        assert_eq!(stats["bin.dat"], (0, 0, true));
        assert_eq!(stats["new.ts"], (0, 0, false));
        assert_eq!(stats["gone.md"], (0, 12, false));
    }

    #[test]
    fn hash_guard() {
        assert!(validate_hash("fa35a6f").is_ok());
        assert!(validate_hash("fa35a6fc80df2abdb3fbec0bee01ceb650533b7f").is_ok());
        assert!(validate_hash("--all").is_err());
        assert!(validate_hash("HEAD~1").is_err());
        assert!(validate_hash("abc").is_err());
    }
}
