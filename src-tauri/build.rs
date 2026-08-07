use std::process::Command;

/// Run a git command in the repo and return trimmed stdout, or None.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Embed git-derived build info so the app version bumps automatically with
/// every commit (no manual Cargo.toml edits) and the running app carries its
/// own changelog. Emits:
///   THREADKNOT_VERSION        "0.1.<commit count>"  (falls back to CARGO_PKG_VERSION)
///   THREADKNOT_GIT_HASH       short HEAD hash        ("dev" without git)
///   THREADKNOT_BUILD_DATE     HEAD commit date       ("" without git)
///   THREADKNOT_CHANGELOG_JSON compact JSON array of recent commits, newest first:
///                         [{version, hash, date, subject, body}, ...]
fn emit_git_info() {
    // Re-run when HEAD moves: new commit, branch switch, fetch that packs refs.
    // Also watch the canonical master ref directly so a `git fetch`/`git pull`
    // that only advances origin/master (without moving HEAD) still regenerates
    // the version.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
    println!("cargo:rerun-if-changed=../.git/packed-refs");
    println!("cargo:rerun-if-changed=../.git/refs/remotes/origin/master");
    println!("cargo:rerun-if-changed=../.git/refs/heads/master");

    let base = env!("CARGO_PKG_VERSION"); // e.g. "0.1.0"
    let majmin = base.rsplit_once('.').map(|(a, _)| a).unwrap_or(base); // "0.1"

    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "dev".into());
    let date = git(&["log", "-1", "--date=short", "--pretty=%ad"]).unwrap_or_default();

    // The primary version number is the commit count of the canonical master
    // line, NOT of the local HEAD. This makes the number universal: every
    // machine that has fetched the same origin/master derives the same
    // "0.1.<n>", regardless of which feature branch is checked out or how many
    // local/merge commits it carries. Prefer the remote ref (local master can
    // be stale), fall back to local master, then HEAD as a last resort.
    let master_ref = ["origin/master", "master", "HEAD"]
        .into_iter()
        .find(|r| git(&["rev-parse", "--verify", "--quiet", r]).is_some())
        .unwrap_or("HEAD");

    let master_count: Option<u64> =
        git(&["rev-list", "--count", master_ref]).and_then(|s| s.parse().ok());

    let version = match master_count {
        None => base.to_string(), // no usable git info: fall back to Cargo version
        Some(n) => {
            let base_ver = format!("{majmin}.{n}");
            let head_sha = git(&["rev-parse", "HEAD"]);
            let master_sha = git(&["rev-parse", master_ref]);
            match (head_sha, master_sha) {
                // Clean release: HEAD is exactly the canonical master tip.
                (Some(h), Some(m)) if h == m => base_ver,
                // HEAD diverges from master: mark it as a dev build so an old or
                // feature checkout can never masquerade as the clean release.
                _ => {
                    let ahead: Option<u64> =
                        git(&["rev-list", "--count", &format!("{master_ref}..HEAD")])
                            .and_then(|s| s.parse().ok());
                    match ahead {
                        Some(a) if a > 0 => format!("{base_ver}-dev{a}"),
                        // Ahead count is zero (behind/divergent) or unknowable:
                        // still a dev build, just without a commit count.
                        _ => format!("{base_ver}-dev"),
                    }
                }
            }
        }
    };

    // Internal git-log changelog (secondary to the user-facing CHANGELOG.md).
    // Its per-commit labels remain based on the local HEAD commit count, exactly
    // as before; the universal number above only governs THREADKNOT_VERSION.
    let count: Option<u64> = git(&["rev-list", "--count", "HEAD"]).and_then(|s| s.parse().ok());

    // Last 60 commits with unit/record separators so subjects and multi-line
    // bodies can't collide with the delimiters.
    let changelog = match (count, git(&["log", "-60", "--date=short", "--pretty=%h\u{1f}%ad\u{1f}%s\u{1f}%b\u{1e}"])) {
        (Some(total), Some(raw)) => {
            let entries: Vec<serde_json::Value> = raw
                .split('\u{1e}')
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .enumerate()
                .map(|(i, rec)| {
                    let mut f = rec.split('\u{1f}');
                    serde_json::json!({
                        "version": format!("{majmin}.{}", total - i as u64),
                        "hash": f.next().unwrap_or("").trim(),
                        "date": f.next().unwrap_or("").trim(),
                        "subject": f.next().unwrap_or("").trim(),
                        "body": f.next().unwrap_or("").trim(),
                    })
                })
                .collect();
            serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into())
        }
        _ => "[]".into(),
    };

    // The source checkout this binary was compiled from, so the running app can
    // ask git whether a newer master exists. Empty for builds made outside a
    // checkout (packaged installs), which disables the update tab entirely.
    let repo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .filter(|p| p.join(".git").exists())
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    println!("cargo:rustc-env=THREADKNOT_VERSION={version}");
    println!("cargo:rustc-env=THREADKNOT_GIT_HASH={hash}");
    println!("cargo:rustc-env=THREADKNOT_BUILD_DATE={date}");
    println!("cargo:rustc-env=THREADKNOT_REPO_DIR={repo_dir}");
    // Compact JSON is newline-free (bodies are escaped), so it is safe as a
    // single-line rustc-env value.
    println!("cargo:rustc-env=THREADKNOT_CHANGELOG_JSON={changelog}");
}

/// Parse the client-facing CHANGELOG.md into JSON releases:
///   [{version, date, notes: [string]}]
/// Headers look like "## v0.1.45 · 2026-07-23"; bullets are "- " lines.
/// This is what the version popover shows users — the raw git log stays
/// internal.
fn emit_release_notes() {
    println!("cargo:rerun-if-changed=../CHANGELOG.md");

    let mut releases: Vec<serde_json::Value> = Vec::new();
    if let Ok(text) = std::fs::read_to_string("../CHANGELOG.md") {
        for line in text.lines() {
            let line = line.trim();
            if let Some(header) = line.strip_prefix("## ") {
                let version = header
                    .split_whitespace()
                    .find(|t| t.starts_with('v') && t[1..].starts_with(|c: char| c.is_ascii_digit()))
                    .map(|t| t.trim_start_matches('v'))
                    .unwrap_or("")
                    .to_string();
                let date = header
                    .split_whitespace()
                    .find(|t| t.len() == 10 && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-')
                    .unwrap_or("")
                    .to_string();
                releases.push(serde_json::json!({
                    "version": version,
                    "date": date,
                    "notes": [],
                }));
            } else if let Some(bullet) = line.strip_prefix("- ") {
                if let Some(last) = releases.last_mut() {
                    if let Some(notes) = last["notes"].as_array_mut() {
                        notes.push(serde_json::json!(bullet.trim()));
                    }
                }
            }
        }
    }
    let json = serde_json::to_string(&releases).unwrap_or_else(|_| "[]".into());
    println!("cargo:rustc-env=THREADKNOT_RELEASE_NOTES_JSON={json}");
}

fn main() {
    emit_git_info();
    emit_release_notes();
    tauri_build::build()
}
