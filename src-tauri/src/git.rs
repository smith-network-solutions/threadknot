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
            let binary = unified.contains("Binary files ") && unified.lines().count() <= 3;
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
            Ok(json!({ "path": path, "unified": unified, "truncated": truncated, "binary": binary }))
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
}
