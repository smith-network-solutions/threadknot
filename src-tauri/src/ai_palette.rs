//! AI-designed theme palettes for the appearance studio.
//!
//! The studio lets a user drop in a wallpaper and asks the local Claude CLI to
//! design a complementary chat-app color scheme from it. Threadknot already drives
//! that binary for chat (see `agents/claude.rs`); here it is a one-shot,
//! non-interactive shell-out modeled on `dictation.rs`: decode the image to a
//! temp file, hand the CLI a single prompt, parse the JSON it prints back, and
//! validate every field server-side so a malformed answer never reaches the UI.
//! The temp file is always removed, on success or failure.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use crate::protocol::new_id;

/// Hard cap on the incoming data URL. The client sends an already-downscaled
/// image; this only guards against a hand-written or hostile RPC.
const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;
/// Longest free-text steering hint we forward into the prompt.
const MAX_HINT_CHARS: usize = 200;
/// Trimmed length of the AI's suggested theme name.
const MAX_NAME_CHARS: usize = 40;
/// Whole-call ceiling on the CLI. A palette normally lands in a few seconds;
/// past this the process is killed (`kill_on_drop`) and a readable error surfaces.
const CLI_TIMEOUT: Duration = Duration::from_secs(90);

/// The 10 neutral slots, keyed exactly like `CustomTheme.colors`.
const SLOTS: [&str; 10] = [
    "bg", "bg-raise", "panel", "panel-2", "panel-3", "line", "line-2", "text", "dim", "faint",
];

/// RAII cleanup for the scratch wallpaper. `Drop` removes the file, so panic,
/// timeout, and early-return paths all attempt cleanup — not just the happy
/// path. On Windows a surviving descendant of the CLI can still hold the image
/// open (`kill_on_drop` reaps only the direct child), so a single retry after a
/// short sleep clears the common sharing-violation race; failures are ignored
/// (temp files are swept by the OS eventually). A future hardening could place
/// the CLI in a Job Object so every descendant dies with the parent, closing
/// the handle before cleanup runs.
struct TempFile {
    path: PathBuf,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if std::fs::remove_file(&self.path).is_ok() {
            return;
        }
        // A descendant may still hold the handle for a beat after the parent
        // was killed; give Windows a moment, then try once more.
        std::thread::sleep(Duration::from_millis(50));
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A palette designed by the AI. Mirrors the `AiPalette` interface in
/// `src/lib/protocol.ts`.
#[derive(Debug, Serialize)]
pub struct AiPalette {
    /// "dark" | "light" — which family the scheme is built for.
    pub family: String,
    pub accent: String,
    /// The 10 neutral slots keyed like `CustomTheme.colors`.
    pub colors: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Ask the local Claude CLI to design a palette that complements `image_data_url`.
pub async fn generate(image_data_url: &str, hint: Option<&str>) -> Result<AiPalette> {
    anyhow::ensure!(
        image_data_url.len() <= MAX_IMAGE_BYTES,
        "the wallpaper is too large to analyze (over 3 MB) — it should already be downscaled"
    );

    // Availability first, so a machine with no CLI fails fast and cleanly.
    let bin = crate::agents::resolve_bin("claude")
        .ok_or_else(|| anyhow!("the Claude CLI is not installed on this machine"))?;

    let hint = hint
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(|h| h.chars().take(MAX_HINT_CHARS).collect::<String>());

    let (ext, bytes) = decode_data_url(image_data_url)?;

    // Write to the system temp dir, then set that as the CLI's working directory
    // so the relative-or-absolute Read stays inside a trusted root.
    let dir = std::env::temp_dir();
    let path = dir.join(format!("threadknot-ai-palette-{}.{ext}", new_id()));
    std::fs::write(&path, &bytes).context("could not stage the wallpaper for analysis")?;

    // The guard deletes the scratch file on every exit path (success, error,
    // timeout, panic) — its Drop is the single point of cleanup now.
    let guard = TempFile { path };
    let result = invoke(&bin, &dir, &guard.path, hint.as_deref()).await;
    drop(guard);
    result
}

/// Split a `data:image/...;base64,...` URL into (file extension, raw bytes).
/// Only PNG and JPEG are accepted; anything else is rejected readably.
fn decode_data_url(url: &str) -> Result<(&'static str, Vec<u8>)> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| anyhow!("the wallpaper is not a valid image data URL"))?;
    let (meta, data) = rest
        .split_once(',')
        .ok_or_else(|| anyhow!("the wallpaper is not a valid image data URL"))?;
    anyhow::ensure!(
        meta.contains(";base64"),
        "the wallpaper must be a base64-encoded image"
    );
    let mime = meta.split(';').next().unwrap_or("").trim();
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        _ => anyhow::bail!("only PNG and JPEG wallpapers can be analyzed"),
    };
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| anyhow!("the wallpaper image could not be decoded"))?;
    anyhow::ensure!(!bytes.is_empty(), "the wallpaper image is empty");
    Ok((ext, bytes))
}

/// Compose the single prompt string handed to `claude -p`.
fn build_prompt(image: &Path, hint: Option<&str>) -> String {
    let mut prompt = format!(
        "Read the image at {path}. Design a UI color scheme that complements it for a chat app. \
         Respond with ONLY a JSON object, no prose, no code fences: \
         {{\"family\": \"dark\" or \"light\" (dark unless the image is overwhelmingly bright), \
         \"name\": short evocative theme name (2-3 words), \
         \"accent\": vivid hex drawn from or complementing the image, \
         \"colors\": {{\"bg\": deepest background hex, \"bg-raise\": slightly lifted, \
         \"panel\": card surface, \"panel-2\": raised card, \"panel-3\": highest surface, \
         \"line\": subtle border, \"line-2\": stronger border, \
         \"text\": primary text with strong contrast on bg, \"dim\": secondary text, \
         \"faint\": tertiary text}}. All values 6-digit lowercase hex. \
         For a dark family: bg lightness under 12%, panels stepping up gradually, \
         text near-white tinted toward the image's mood. Ensure WCAG-ish contrast: \
         text vs bg >= 10:1, dim >= 5:1.",
        path = image.display()
    );
    if let Some(hint) = hint {
        prompt.push_str(&format!(" User direction: {hint}."));
    }
    prompt
}

/// Run the CLI one-shot and turn its stdout into a validated palette.
async fn invoke(bin: &Path, cwd: &Path, image: &Path, hint: Option<&str>) -> Result<AiPalette> {
    let prompt = build_prompt(image, hint);

    let mut cmd = Command::new(bin);
    cmd.current_dir(cwd)
        // A desktop launch inherits almost none of the shell PATH; the agent
        // PATH is what lets the CLI find node and itself.
        .env("PATH", crate::agents::agent_path())
        .args([
            "-p",
            prompt.as_str(),
            "--output-format",
            "text",
            "--allowedTools",
            "Read",
            "--max-turns",
            "3",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // If the timeout drops this future, the child (and its wait) go with it.
        .kill_on_drop(true);
    crate::agents::no_console(&mut cmd);

    let child = cmd.spawn().context("could not start the Claude CLI")?;
    let output = match tokio::time::timeout(CLI_TIMEOUT, child.wait_with_output()).await {
        Ok(res) => res.context("the Claude CLI failed while designing a palette")?,
        Err(_) => return Err(anyhow!("the AI took too long to design a palette — try again")),
    };

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let tail = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("the Claude CLI exited with an error");
        return Err(anyhow!("the AI could not design a palette: {tail}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_palette(&stdout)
}

/// The byte offset of the `}` that closes the `{` at `start`, tracking nesting
/// while ignoring braces inside JSON strings (and their `\"` escapes). `None` if
/// the object never closes.
fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth: usize = 0;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every balanced top-level `{ ... }` object in the CLI's text output, in order,
/// ignoring stray prose or code fences that slipped past the "ONLY a JSON
/// object" instruction. The model sometimes emits more than one brace fragment
/// (a schema echo or example next to the real answer), so callers try each
/// candidate; the first entry preserves the old "first `{`" behavior for the
/// common single-object case. Brace/quote indices are ASCII, so the byte slices
/// always fall on char boundaries even with multi-byte prose in between.
fn json_object_candidates(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = balanced_end(bytes, i) {
                out.push(&s[i..=end]);
                i = end + 1;
                continue;
            }
            // An unbalanced trailing `{` can't yield any object after it either.
            break;
        }
        i += 1;
    }
    out
}

/// Normalize a hex color to `#rrggbb` lowercase, or `None` if it is not exactly
/// six hex digits (with or without a leading `#`).
fn norm_hex(v: &str) -> Option<String> {
    let v = v.trim();
    // Accept the value with or without a leading `#`; the normalized output
    // always carries one, so it matches `^#[0-9a-f]{6}$`.
    let hex = v.strip_prefix('#').unwrap_or(v);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", hex.to_ascii_lowercase()))
}

const UNUSABLE: &str = "the AI returned an unusable palette, try again";

/// Parse + validate the model's JSON into an [`AiPalette`]. Every failure mode
/// collapses to one readable message; nothing malformed reaches the client.
/// Tries each balanced object the output carries and accepts the first that
/// validates, so a schema echo or example object beside the real answer is
/// skipped rather than fatal.
fn parse_palette(stdout: &str) -> Result<AiPalette> {
    let candidates = json_object_candidates(stdout);
    anyhow::ensure!(!candidates.is_empty(), "{UNUSABLE}");
    for candidate in candidates {
        if let Ok(palette) = validate_palette(candidate) {
            return Ok(palette);
        }
    }
    Err(anyhow!(UNUSABLE))
}

/// Validate a single JSON object string against the palette schema.
fn validate_palette(json: &str) -> Result<AiPalette> {
    let value: Value = serde_json::from_str(json).map_err(|_| anyhow!(UNUSABLE))?;

    let family = value
        .get("family")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    anyhow::ensure!(family == "dark" || family == "light", "{UNUSABLE}");

    let accent = value
        .get("accent")
        .and_then(Value::as_str)
        .and_then(norm_hex)
        .ok_or_else(|| anyhow!(UNUSABLE))?;

    let obj = value
        .get("colors")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!(UNUSABLE))?;
    let mut colors = BTreeMap::new();
    for slot in SLOTS {
        let hex = obj
            .get(slot)
            .and_then(Value::as_str)
            .and_then(norm_hex)
            .ok_or_else(|| anyhow!(UNUSABLE))?;
        colors.insert(slot.to_string(), hex);
    }

    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(MAX_NAME_CHARS).collect::<String>());

    Ok(AiPalette {
        family,
        accent,
        colors,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_png_and_jpeg_data_urls() {
        // "hi" base64-encoded is "aGk=".
        let (ext, bytes) = decode_data_url("data:image/png;base64,aGk=").unwrap();
        assert_eq!(ext, "png");
        assert_eq!(bytes, b"hi");
        let (ext, _) = decode_data_url("data:image/jpeg;base64,aGk=").unwrap();
        assert_eq!(ext, "jpg");
    }

    #[test]
    fn rejects_non_image_and_malformed_urls() {
        assert!(decode_data_url("data:image/gif;base64,aGk=").is_err());
        assert!(decode_data_url("data:image/png,aGk=").is_err());
        assert!(decode_data_url("not-a-data-url").is_err());
    }

    #[test]
    fn normalizes_hex_and_rejects_junk() {
        assert_eq!(norm_hex("#AABBCC").as_deref(), Some("#aabbcc"));
        assert_eq!(norm_hex("112233").as_deref(), Some("#112233"));
        assert_eq!(norm_hex("#12g"), None);
        assert_eq!(norm_hex("#1234567"), None);
    }

    #[test]
    fn parses_a_well_formed_palette_ignoring_fences() {
        let out = "```json\n{\"family\":\"Dark\",\"name\":\"Ocean Deep\",\"accent\":\"#5B8DEF\",\
                   \"colors\":{\"bg\":\"#0a0e14\",\"bg-raise\":\"#0f1420\",\"panel\":\"#141a28\",\
                   \"panel-2\":\"#1a2130\",\"panel-3\":\"#212a3b\",\"line\":\"#2a3444\",\
                   \"line-2\":\"#3a4658\",\"text\":\"#eef2f8\",\"dim\":\"#a7b2c4\",\
                   \"faint\":\"#5f6b7e\"}}\n```";
        let palette = parse_palette(out).unwrap();
        assert_eq!(palette.family, "dark");
        assert_eq!(palette.accent, "#5b8def");
        assert_eq!(palette.name.as_deref(), Some("Ocean Deep"));
        assert_eq!(palette.colors.len(), 10);
        assert_eq!(palette.colors.get("text").unwrap(), "#eef2f8");
    }

    #[test]
    fn skips_prose_and_an_example_object_to_reach_the_real_palette() {
        // Prose, then a schema-shaped example whose values are placeholders (so
        // it fails validation), then the real object. The scanner must try each
        // and accept the last one.
        let out = "Sure — here's the shape I'll return: \
                   {\"family\": \"dark or light\", \"accent\": \"vivid hex\", \
                   \"colors\": {\"bg\": \"deepest background hex\"}}. \
                   And here is the palette:\n\
                   {\"family\":\"dark\",\"name\":\"Ember Dusk\",\"accent\":\"#d9a35c\",\
                   \"colors\":{\"bg\":\"#0a0e14\",\"bg-raise\":\"#0f1420\",\"panel\":\"#141a28\",\
                   \"panel-2\":\"#1a2130\",\"panel-3\":\"#212a3b\",\"line\":\"#2a3444\",\
                   \"line-2\":\"#3a4658\",\"text\":\"#eef2f8\",\"dim\":\"#a7b2c4\",\
                   \"faint\":\"#5f6b7e\"}}";
        let palette = parse_palette(out).unwrap();
        assert_eq!(palette.family, "dark");
        assert_eq!(palette.accent, "#d9a35c");
        assert_eq!(palette.name.as_deref(), Some("Ember Dusk"));
        assert_eq!(palette.colors.len(), 10);
        assert_eq!(palette.colors.get("faint").unwrap(), "#5f6b7e");
    }

    #[test]
    fn candidate_scanner_ignores_braces_inside_strings() {
        // A brace inside a JSON string value must not break balance tracking.
        let candidates = json_object_candidates("{\"a\":\"a } b { c\"} tail {\"b\":1}");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0], "{\"a\":\"a } b { c\"}");
        assert_eq!(candidates[1], "{\"b\":1}");
    }

    #[test]
    fn rejects_missing_slots_and_bad_family() {
        // Missing the "faint" slot.
        let missing = "{\"family\":\"dark\",\"accent\":\"#5b8def\",\"colors\":{\"bg\":\"#0a0e14\"}}";
        assert!(parse_palette(missing).is_err());
        // Family must be dark|light.
        let bad_family = "{\"family\":\"neon\",\"accent\":\"#5b8def\",\"colors\":{}}";
        assert!(parse_palette(bad_family).is_err());
        // No JSON at all.
        assert!(parse_palette("sorry, I cannot do that").is_err());
    }
}
