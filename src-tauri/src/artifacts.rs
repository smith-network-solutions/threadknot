//! Agent artifact detection: which produced files count as "deliverables", and
//! a bounded scan of the project tree used to diff them across a turn.
//!
//! Artifacts have two channels. The PRIMARY one is explicit: agents register
//! deliverables through the `publish_artifact` MCP tool (`mcp.rs` →
//! `AgentHub::publish_artifact`), the pattern every serious agent product uses
//! (Claude.ai's artifacts tool, Manus's attach-deliverables rule, Codex's
//! registered image outputs). This module implements the conservative FALLBACK:
//! the hub (`agents::mod`) snapshots candidate files when a turn starts
//! (`UserMessage`) and diffs at the boundary (`TurnCompleted`/`TurnAborted`);
//! [`select_artifacts`] keeps only files the turn plausibly produced *for the
//! user* — newly created deliverable formats backed by an explicit mention —
//! so attachments, source edits, and build fan-out never become artifacts.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Directory names never scanned for artifacts (build output / vcs / deps).
/// `.threadknot` matters most: user attachments are materialized into
/// `<cwd>/.threadknot/attachments/` mid-turn, so without the exclusion a PDF the
/// *user sent* would diff as a freshly produced deliverable. `.armada` stays on
/// the list permanently — every workspace Armada ever touched still holds one,
/// and dropping it would surface those old attachments as new deliverables.
const EXCLUDED_DIRS: [&str; 10] = [
    ".git",
    ".threadknot",
    ".armada",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "__pycache__",
];

/// Cap on files a single scan considers (keeps a huge tree bounded).
const MAX_SCAN: usize = 25_000;

/// Wall-clock ceiling on a single scan. The file cap alone doesn't bound a
/// scan on a slow filesystem (e.g. an sshfs/NFS mount where every `stat` is a
/// network round-trip) — reaching 25k files there can take ~30s, and this runs
/// synchronously on the turn's critical path (baseline at turn start, diff at
/// turn end). Bailing early yields a partial snapshot: at worst a few spurious
/// or missed artifact cards on a pathological remote project, which is far
/// better than freezing every turn for half a minute.
const SCAN_BUDGET: Duration = Duration::from_millis(750);

/// Largest file we will snapshot as an artifact (100 MiB).
pub const MAX_ARTIFACT_BYTES: u64 = 100 * 1024 * 1024;

/// Never emit an unbounded wall of artifact cards from a generator/build. If a
/// turn exceeds this threshold, only explicitly named outputs survive.
const MAX_AUTOMATIC_ARTIFACTS: usize = 8;

/// A scan result: deliverable rel_path (POSIX) → (mtime, size).
pub type DeliverableSnapshot = HashMap<String, (SystemTime, u64)>;

/// Deliverable file extensions → MIME type. A produced file counts as an
/// artifact only if its extension is here; routine source-code edits are
/// deliberately excluded so the Artifacts tab stays a list of outputs.
pub fn deliverable_mime(ext: &str) -> Option<&'static str> {
    let mime = match ext {
        // documents
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        // data / text deliverables
        "csv" => "text/csv; charset=utf-8",
        "tsv" => "text/tab-separated-values; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        // images
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        // video — recorded walkthroughs (see `recorder.rs`) land here, and the
        // viewers play these inline rather than offering a download.
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        // archives
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" | "tgz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        _ => return None,
    };
    Some(mime)
}

/// Lowercase extension of a path, or "" if none.
pub fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Formats that are overwhelmingly standalone deliverables. Text, web, data,
/// and image formats are intentionally absent: those are also common project
/// inputs/build outputs and need conversational evidence before promotion.
fn is_high_confidence_extension(ext: &str) -> bool {
    matches!(
        ext,
        "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "rtf"
            | "epub"
            | "zip"
            | "tar"
            | "gz"
            | "tgz"
            | "7z"
    )
}

fn path_is_mentioned(text: &str, rel: &str, basename_is_unique: bool) -> bool {
    let rel = rel.replace('\\', "/").to_ascii_lowercase();
    if text.contains(&rel) {
        return true;
    }
    basename_is_unique
        && Path::new(&rel)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| text.contains(name))
}

/// A turn-diff candidate: a deliverable-typed file that appeared or changed
/// between the turn's baseline and its boundary.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Project-relative POSIX path.
    pub rel: String,
    /// The file already existed at turn start (the agent modified it rather
    /// than creating it).
    pub existed: bool,
    /// The path is already an artifact of this thread (published or detected
    /// on an earlier turn) — a mentioned re-write refreshes it.
    pub known: bool,
}

/// Reduce a turn-wide filesystem diff to files that plausibly represent a
/// deliverable for the user. This is the conservative FALLBACK behind the
/// `publish_artifact` MCP tool — when in doubt, drop the file: a missed
/// artifact is recoverable (the agent can publish it, the user has the Files
/// tab), while a spurious one teaches the user to ignore the Artifacts tab.
///
/// Rules:
/// - Only newly CREATED files are ever automatic. Edits to pre-existing files
///   are code/content changes, not deliverables — a modified file survives
///   only when the agent's final prose explicitly points at it AND it is
///   either a high-confidence document format (a regenerated `report.pdf`) or
///   already an artifact of this thread (refreshing an established
///   deliverable).
/// - A created binary document/archive (`pdf`, `docx`, `zip`, …) counts on
///   its own: those formats are overwhelmingly standalone outputs.
/// - A created ambiguous project format (`.md`, `.html`, `.json`, images,
///   CSV, …) counts only when the exact file is named — by the user's request
///   or by the agent's final prose.
/// - Large fan-outs (doc builds, generators) require the agent's prose to name
///   every retained file regardless of type.
pub fn select_artifacts(
    candidates: Vec<Candidate>,
    user_text: &str,
    assistant_text: &str,
) -> Vec<String> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let user = user_text.replace('\\', "/").to_ascii_lowercase();
    let assistant = assistant_text.replace('\\', "/").to_ascii_lowercase();

    let mut basename_counts: HashMap<String, usize> = HashMap::new();
    for c in &candidates {
        if let Some(name) = Path::new(&c.rel).file_name().and_then(|name| name.to_str()) {
            *basename_counts
                .entry(name.to_ascii_lowercase())
                .or_default() += 1;
        }
    }

    let mentions = |text: &str, rel: &str| {
        let basename = Path::new(rel)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(rel)
            .to_ascii_lowercase();
        let unique = basename_counts.get(&basename).copied().unwrap_or(0) == 1;
        path_is_mentioned(text, rel, unique)
    };

    let bulk = candidates.len() > MAX_AUTOMATIC_ARTIFACTS;
    let mut selected = candidates
        .into_iter()
        .filter(|c| {
            let named_by_user = mentions(&user, &c.rel);
            let presented_by_assistant = mentions(&assistant, &c.rel);
            let high_confidence = is_high_confidence_extension(&ext_of(Path::new(&c.rel)));
            if bulk {
                // Build instructions often name representative inputs (for
                // example "page-1.html through page-50.html"). In a bulk turn,
                // only the agent's final prose can promote a particular file.
                return (!c.existed || c.known) && presented_by_assistant;
            }
            if c.existed {
                return (high_confidence || c.known) && presented_by_assistant;
            }
            high_confidence || named_by_user || presented_by_assistant
        })
        .map(|c| c.rel)
        .collect::<Vec<_>>();

    // Even a response that enumerates a giant generated tree should not flood
    // the transcript or snapshot storage.
    selected.truncate(MAX_AUTOMATIC_ARTIFACTS);
    selected
}

/// Snapshot of deliverable files under `root`: rel_path (POSIX) → (mtime, size).
/// BFS, excluding build/vcs/dep dirs, never following symlinks, bounded.
pub fn scan_deliverables(root: &Path) -> DeliverableSnapshot {
    let mut out: DeliverableSnapshot = HashMap::new();
    let mut queue: VecDeque<(PathBuf, String)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new()));

    let started = Instant::now();
    while let Some((dir, prefix)) = queue.pop_front() {
        if out.len() >= MAX_SCAN {
            break;
        }
        // Slow-filesystem guard: the file cap can't bound a mount where every
        // directory read blocks on the network. Stop with a partial snapshot
        // rather than stall the turn (checked per directory — the expensive
        // syscalls all sit inside the loop below).
        if started.elapsed() >= SCAN_BUDGET {
            tracing::warn!(
                "artifact scan of {} hit {}ms budget after {} files; partial snapshot",
                dir.display(),
                SCAN_BUDGET.as_millis(),
                out.len(),
            );
            break;
        }
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if ft.is_dir() {
                if EXCLUDED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                queue.push_back((entry.path(), rel));
            } else if ft.is_file() {
                if deliverable_mime(&ext_of(Path::new(&name))).is_none() {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                out.insert(rel, (mtime, meta.len()));
                if out.len() >= MAX_SCAN {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{select_artifacts, Candidate};

    fn created(rel: &str) -> Candidate {
        Candidate {
            rel: rel.into(),
            existed: false,
            known: false,
        }
    }

    fn modified(rel: &str) -> Candidate {
        Candidate {
            rel: rel.into(),
            existed: true,
            known: false,
        }
    }

    fn known(rel: &str) -> Candidate {
        Candidate {
            rel: rel.into(),
            existed: true,
            known: true,
        }
    }

    #[test]
    fn suppresses_docs_build_fanout() {
        let changed = (0..30)
            .map(|n| created(&format!("generated-docs/page-{n}.html")))
            .collect();
        assert!(select_artifacts(changed, "build the docs", "Docs build passed").is_empty());
    }

    #[test]
    fn bulk_build_paths_named_only_by_user_are_not_deliverables() {
        let mut changed = (1..=12)
            .map(|n| created(&format!("generated-docs/page-{n}.html")))
            .collect::<Vec<_>>();
        changed.push(created("report.csv"));
        assert_eq!(
            select_artifacts(
                changed,
                "Build page-1.html through page-12.html and export report.csv.",
                "`report.csv` is ready for download.",
            ),
            vec!["report.csv"]
        );
    }

    #[test]
    fn created_ambiguous_formats_need_an_explicit_mention() {
        // Created but never named by anyone: internal scratch, not a deliverable.
        assert!(select_artifacts(
            vec![created("notes/scratch.md")],
            "clean up the api layer",
            "Refactored the api layer.",
        )
        .is_empty());
        // Named by the agent's final prose: promoted.
        assert_eq!(
            select_artifacts(
                vec![created("reports/summary.html")],
                "make me a customer report",
                "The report is ready at `reports/summary.html`.",
            ),
            vec!["reports/summary.html"]
        );
        // Named by the user's request: promoted even if the agent stays terse.
        assert_eq!(
            select_artifacts(
                vec![created("exports/customers.csv")],
                "export the data to exports/customers.csv",
                "Done.",
            ),
            vec!["exports/customers.csv"]
        );
    }

    #[test]
    fn edits_to_existing_files_are_not_deliverables() {
        // The classic false positive: the agent edited an html/source file the
        // user already had. Never an artifact, no matter what the prose says.
        assert!(select_artifacts(
            vec![modified("site/index.html")],
            "fix the download button on the landing page",
            "Updated `site/index.html` — the download link now works.",
        )
        .is_empty());
        // A regenerated high-confidence document the agent points at survives.
        assert_eq!(
            select_artifacts(
                vec![modified("out/brief.pdf")],
                "regenerate the brief with the new numbers",
                "Rebuilt `out/brief.pdf` with the Q3 figures.",
            ),
            vec!["out/brief.pdf"]
        );
        // ...but not when it changed silently (background rebuild, watcher).
        assert!(select_artifacts(
            vec![modified("out/brief.pdf")],
            "tweak the styling",
            "Adjusted the stylesheet.",
        )
        .is_empty());
    }

    #[test]
    fn established_artifacts_refresh_on_a_mentioned_rewrite() {
        // Once a path is an artifact of the thread, a re-generation the agent
        // points at updates it — even for ambiguous formats like CSV.
        assert_eq!(
            select_artifacts(
                vec![known("report.csv")],
                "update the report",
                "The updated report is in `report.csv`.",
            ),
            vec!["report.csv"]
        );
        // A silent change to an established artifact still stays out.
        assert!(select_artifacts(
            vec![known("report.csv")],
            "refactor the exporter",
            "Refactored the exporter module.",
        )
        .is_empty());
    }

    #[test]
    fn created_standalone_documents_remain_automatic() {
        assert_eq!(
            select_artifacts(vec![created("out/brief.pdf")], "prepare the brief", "Done."),
            vec!["out/brief.pdf"]
        );
    }

    #[test]
    fn unnamed_single_data_output_is_dropped() {
        // Tightened from the old behavior: with no mention from either side,
        // even a lone CSV stays out — the agent should publish_artifact it.
        assert!(select_artifacts(
            vec![created("exports/customers.csv")],
            "export the customers",
            "Done.",
        )
        .is_empty());
    }
}
