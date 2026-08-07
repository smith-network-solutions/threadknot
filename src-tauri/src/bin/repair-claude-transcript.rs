//! Opt-in, targeted repair for Claude CLI transcripts poisoned with empty text
//! blocks (see `threadknot_lib::agents::repair`). Pass one or more transcript
//! `.jsonl` paths explicitly; nothing is scanned or rewritten unless named.
//!
//!   repair-claude-transcript <session.jsonl> [more.jsonl ...]
//!
//! Each file is backed up (`<path>.bak-<millis>`) before an atomic rewrite, and
//! only poisoned records change. Running it again is a no-op.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: repair-claude-transcript <session.jsonl> [more.jsonl ...]");
        return ExitCode::FAILURE;
    }

    let mut scanned = 0usize;
    let mut repaired = 0usize;
    let mut total_records = 0usize;
    let mut total_blocks = 0usize;
    let mut failed = false;

    for path in &paths {
        scanned += 1;
        match threadknot_lib::agents::repair::repair_file(path) {
            Ok(report) => {
                if report.changed() {
                    repaired += 1;
                    total_records += report.records_repaired;
                    total_blocks += report.blocks_removed;
                    println!(
                        "repaired {}: {} record(s), {} empty text block(s) removed; backup {}",
                        path.display(),
                        report.records_repaired,
                        report.blocks_removed,
                        report
                            .backup
                            .as_ref()
                            .map(|b| b.display().to_string())
                            .unwrap_or_default()
                    );
                } else {
                    println!("clean {} (no poisoned records)", path.display());
                }
            }
            Err(error) => {
                failed = true;
                eprintln!("error {}: {error}", path.display());
            }
        }
    }

    println!(
        "summary: {scanned} file(s) scanned, {repaired} repaired, {total_records} record(s), {total_blocks} block(s) removed"
    );

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
