//! Idempotent repair for Claude CLI transcripts poisoned with empty text blocks.
//!
//! Legacy threads recorded before the [`super::content`] guard existed can hold
//! user records whose `message.content` array carries an empty
//! `{"type":"text","text":""}` beside a valid image (or other non-text) block.
//! The Claude CLI replays that transcript verbatim on every `--resume`, so the
//! thread fails forever with `messages: text content blocks must be non-empty`.
//!
//! This repair drops ONLY the empty text blocks from those specific records.
//! Healthy lines are passed through byte-for-byte, so the file is otherwise
//! untouched. A backup is written before any change, the write is atomic
//! (temp file + rename), and running it twice yields the same file.

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::content::is_empty_text_block;

/// Outcome of repairing one transcript file.
#[derive(Debug, Default, Clone)]
pub struct RepairReport {
    pub path: PathBuf,
    pub records_repaired: usize,
    pub blocks_removed: usize,
    /// Backup file created before writing, if the file changed.
    pub backup: Option<PathBuf>,
}

impl RepairReport {
    pub fn changed(&self) -> bool {
        self.records_repaired > 0
    }
}

/// Repair a single transcript line. Returns `Some((repaired_line, removed))`
/// only when the line is a user record that has BOTH one or more empty text
/// blocks AND at least one valid non-text block. Every other line (healthy
/// records, non-user records, parse failures) returns `None` and must be kept
/// verbatim by the caller.
pub fn repair_line(line: &str) -> Option<(String, usize)> {
    let mut record: Value = serde_json::from_str(line).ok()?;
    if record.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let blocks = record.pointer("/message/content")?.as_array()?;

    let has_empty_text = blocks.iter().any(is_empty_text_block);
    let has_valid_nontext = blocks.iter().any(|b| {
        b.get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t != "text")
    });
    if !(has_empty_text && has_valid_nontext) {
        return None;
    }

    let mut removed = 0usize;
    let kept: Vec<Value> = blocks
        .iter()
        .filter(|b| {
            if is_empty_text_block(b) {
                removed += 1;
                false
            } else {
                true
            }
        })
        .cloned()
        .collect();
    if removed == 0 {
        return None;
    }
    record["message"]["content"] = Value::Array(kept);
    let repaired = serde_json::to_string(&record).ok()?;
    Some((repaired, removed))
}

/// Repair every poisoned record in `path`. Writes a timestamped backup and
/// updates the file atomically only when something actually changed. A file
/// with no poisoned records is left untouched (no backup, no write).
pub fn repair_file(path: &Path) -> std::io::Result<RepairReport> {
    let original = std::fs::read_to_string(path)?;
    let mut out = String::with_capacity(original.len());
    let mut records_repaired = 0usize;
    let mut blocks_removed = 0usize;

    // Preserve every newline exactly (including a missing trailing newline).
    for chunk in original.split_inclusive('\n') {
        let (body, newline) = match chunk.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (chunk, ""),
        };
        if body.trim().is_empty() {
            out.push_str(chunk);
            continue;
        }
        match repair_line(body) {
            Some((fixed, removed)) => {
                records_repaired += 1;
                blocks_removed += removed;
                out.push_str(&fixed);
                out.push_str(newline);
            }
            None => out.push_str(chunk),
        }
    }

    if records_repaired == 0 {
        return Ok(RepairReport {
            path: path.to_path_buf(),
            ..Default::default()
        });
    }

    let backup = backup_path(path);
    std::fs::write(&backup, &original)?;
    write_atomic(path, &out)?;

    Ok(RepairReport {
        path: path.to_path_buf(),
        records_repaired,
        blocks_removed,
        backup: Some(backup),
    })
}

/// Locate and repair one native Claude session before `--resume`.
///
/// Claude stores transcripts one directory below `<config>/projects`, keyed by
/// session id. Searching that single bounded level avoids depending on Claude's
/// workspace-slug encoding, which has changed across CLI releases.
pub fn repair_session_transcript(session_id: &str) -> std::io::Result<Option<RepairReport>> {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")));
    let Some(config) = config else {
        return Ok(None);
    };
    repair_session_transcript_under(&config, session_id)
}

/// Same repair against an explicit config home — a Claudex profile keeps its
/// transcripts in its own `CLAUDE_CONFIG_DIR`, not the one this process
/// inherited.
pub fn repair_session_transcript_in(
    config: &Path,
    session_id: &str,
) -> std::io::Result<Option<RepairReport>> {
    repair_session_transcript_under(config, session_id)
}

fn repair_session_transcript_under(
    config: &Path,
    session_id: &str,
) -> std::io::Result<Option<RepairReport>> {
    let projects = config.join("projects");
    let name = format!("{session_id}.jsonl");
    let entries = match std::fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let path = entry.path().join(&name);
        if path.is_file() {
            return repair_file(&path).map(Some);
        }
    }
    Ok(None)
}

/// `<path>.bak-<unix_millis>` alongside the original.
fn backup_path(path: &Path) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".bak-{millis}"));
    PathBuf::from(name)
}

/// Write to a sibling temp file then rename over the target, so a crash can
/// never leave a half-written transcript.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp-repair");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn poisoned_user_line() -> String {
        json!({
            "type": "user",
            "uuid": "u-1",
            "timestamp": "2026-07-24T03:26:18.114Z",
            "message": {
                "role": "user",
                "content": [
                    { "type": "text", "text": "" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }
                ]
            }
        })
        .to_string()
    }

    #[test]
    fn repairs_empty_text_beside_image() {
        let (fixed, removed) = repair_line(&poisoned_user_line()).unwrap();
        assert_eq!(removed, 1);
        let v: Value = serde_json::from_str(&fixed).unwrap();
        let content = v.pointer("/message/content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "image");
        // Metadata preserved.
        assert_eq!(v["uuid"], "u-1");
        assert_eq!(v["timestamp"], "2026-07-24T03:26:18.114Z");
        assert_eq!(v["message"]["role"], "user");
    }

    #[test]
    fn healthy_text_image_untouched() {
        let line = json!({
            "type": "user",
            "message": { "role": "user", "content": [
                { "type": "text", "text": "hi" },
                { "type": "image", "source": {} }
            ] }
        })
        .to_string();
        assert!(repair_line(&line).is_none());
    }

    #[test]
    fn text_only_user_record_untouched() {
        // Empty text with no non-text block is out of scope for this repair.
        let line = json!({
            "type": "user",
            "message": { "role": "user", "content": [ { "type": "text", "text": "" } ] }
        })
        .to_string();
        assert!(repair_line(&line).is_none());
    }

    #[test]
    fn non_user_record_untouched() {
        let line = json!({ "type": "assistant", "message": { "content": [] } }).to_string();
        assert!(repair_line(&line).is_none());
    }

    #[test]
    fn tool_result_beside_empty_text_repaired() {
        let line = json!({
            "type": "user",
            "message": { "role": "user", "content": [
                { "type": "text", "text": "  " },
                { "type": "tool_result", "tool_use_id": "t1", "content": "ok" }
            ] }
        })
        .to_string();
        let (fixed, removed) = repair_line(&line).unwrap();
        assert_eq!(removed, 1);
        let v: Value = serde_json::from_str(&fixed).unwrap();
        let content = v.pointer("/message/content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
    }

    #[test]
    fn file_repair_is_idempotent_and_atomic() {
        let dir = std::env::temp_dir().join(format!("threadknot-repair-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");

        let healthy = json!({
            "type": "user",
            "uuid": "healthy",
            "message": { "role": "user", "content": [ { "type": "text", "text": "keep me" } ] }
        })
        .to_string();
        let contents = format!("{healthy}\n{}\n", poisoned_user_line());
        std::fs::write(&path, &contents).unwrap();

        let report = repair_file(&path).unwrap();
        assert_eq!(report.records_repaired, 1);
        assert_eq!(report.blocks_removed, 1);
        assert!(report.backup.as_ref().unwrap().exists());

        // Healthy line preserved byte-for-byte.
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.lines().next().unwrap() == healthy);
        // No empty text blocks remain anywhere.
        for line in after.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            if let Some(arr) = v.pointer("/message/content").and_then(Value::as_array) {
                assert!(!arr.iter().any(is_empty_text_block));
            }
        }

        // Second run is a no-op.
        let again = repair_file(&path).unwrap();
        assert_eq!(again.records_repaired, 0);
        assert!(again.backup.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_session_without_knowing_claudes_workspace_slug() {
        let dir =
            std::env::temp_dir().join(format!("threadknot-repair-session-{}", uuid::Uuid::new_v4()));
        let project = dir.join("projects").join("-a-slug-that-may-change");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("session-123.jsonl");
        std::fs::write(&path, format!("{}\n", poisoned_user_line())).unwrap();

        let report = repair_session_transcript_under(&dir, "session-123")
            .unwrap()
            .unwrap();
        assert_eq!(report.records_repaired, 1);
        let fixed = std::fs::read_to_string(&path).unwrap();
        assert!(!fixed.contains(r#""text":"""#));

        std::fs::remove_dir_all(dir).unwrap();
    }
}
