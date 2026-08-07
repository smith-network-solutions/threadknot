//! Voice dictation for the composer.
//!
//! The webview can't do this itself. WebKitGTK ships `enable-media-stream` off
//! and wry installs no `permission-request` handler, so `getUserMedia` is
//! simply unavailable inside the desktop shell — a browser-side recorder would
//! be dead in the app it's meant for. So Threadknot records the machine's own
//! microphone with ffmpeg (already a runtime dependency for the browser
//! recorder) and transcribes the clip with a local Whisper install. Nothing
//! leaves the machine and no API key is involved.
//!
//! There is one microphone, so there is one recording slot. Starting a second
//! recording discards the first rather than fighting over the device.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

/// Hard cap on a single clip. The client gives up on a request after 30s, and
/// transcription time scales with clip length, so a runaway recording would
/// return nothing usable anyway. Two minutes is far more than one prompt.
const MAX_SECONDS: u32 = 120;

/// Whisper wants 16 kHz mono; giving it exactly that skips a resample.
const SAMPLE_RATE: &str = "16000";

/// Below this mean volume the clip is silence — a dead mic, or the button
/// pressed and released without speaking. Whisper hallucinates confident
/// filler ("You", "Thanks for watching!") on silent input, so it never sees it.
const SILENCE_DBFS: f32 = -50.0;

/// Clips shorter than this can't contain a word worth transcribing.
const MIN_MILLIS: u128 = 400;

/// Phrases Whisper emits from near-silence that survive the volume gate. Any
/// transcript that is *only* one of these is treated as nothing said.
const HALLUCINATIONS: &[&str] = &[
    "you",
    "thank you",
    "thanks for watching",
    "thanks for watching!",
    "bye",
    "so",
    "。",
];

/// A recording in progress: the live ffmpeg plus where it is writing.
struct Active {
    id: String,
    ffmpeg: Child,
    dir: PathBuf,
    wav: PathBuf,
    started: std::time::Instant,
}

#[derive(Default)]
pub struct Dictation {
    /// One mic, one slot.
    active: Mutex<Option<Active>>,
}

/// Absolute path of a tool we shell out to. Resolved against the agent PATH
/// because a desktop launch inherits almost none of the user's shell PATH, and
/// because on Windows that resolution is what appends `.exe`.
fn tool(name: &str) -> Result<PathBuf> {
    crate::agents::resolve_bin(name).ok_or_else(|| anyhow!("{name} is not installed"))
}

/// Why dictation can't run here, or `None` when it can.
fn missing_tool() -> Option<String> {
    if !cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )) {
        return Some("Dictation is only wired up for Windows, macOS and Linux".into());
    }
    if tool("ffmpeg").is_err() {
        return Some("ffmpeg is not installed — dictation records the mic with it".into());
    }
    if tool("whisper").is_err() {
        return Some(
            "Whisper is not installed — run `pip install -U openai-whisper` to dictate".into(),
        );
    }
    None
}

/// `{ available, hint }` for the `hello` payload, so the composer can show the
/// mic button in a state that explains itself.
pub fn capability(master: bool) -> serde_json::Value {
    // A paired phone talking to this machine must not be able to switch on the
    // machine's microphone from across the network, so dictation is master-only.
    if !master {
        return serde_json::json!({
            "available": false,
            "hint": "Dictation records this machine's mic, so it only runs from the app on that machine",
        });
    }
    match missing_tool() {
        None => serde_json::json!({ "available": true }),
        Some(hint) => serde_json::json!({ "available": false, "hint": hint }),
    }
}

/// The device named by `THREADKNOT_MIC_DEVICE`, for when the default is the wrong
/// one (a webcam mic, the wrong input on a dock).
fn mic_override() -> Option<String> {
    std::env::var("THREADKNOT_MIC_DEVICE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// ffmpeg's capture flags for this platform's default input device.
#[cfg(target_os = "linux")]
async fn capture_args() -> Result<(&'static str, String)> {
    Ok(("pulse", mic_override().unwrap_or_else(|| "default".into())))
}

/// ffmpeg's capture flags for this platform's default input device.
#[cfg(target_os = "macos")]
async fn capture_args() -> Result<(&'static str, String)> {
    Ok((
        "avfoundation",
        mic_override().unwrap_or_else(|| ":default".into()),
    ))
}

/// ffmpeg's capture flags for this platform's default input device.
///
/// DirectShow has no notion of a default microphone: every capture has to name
/// a real device, and the names are machine-specific. So ask ffmpeg what
/// Windows can see and record the first microphone in the list.
#[cfg(target_os = "windows")]
async fn capture_args() -> Result<(&'static str, String)> {
    let name = match mic_override() {
        Some(name) => name,
        None => first_dshow_microphone().await?,
    };
    // dshow inputs are typed; `audio=` is what marks this as the mic and not a
    // camera. Tolerate an override that already spells it out.
    let input = if name.starts_with("audio=") || name.starts_with("video=") {
        name
    } else {
        format!("audio={name}")
    };
    Ok(("dshow", input))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows"
)))]
async fn capture_args() -> Result<(&'static str, String)> {
    Err(anyhow!(
        "dictation is only supported on Windows, macOS and Linux"
    ))
}

/// Ask ffmpeg which microphones DirectShow can see and take the first.
#[cfg(target_os = "windows")]
async fn first_dshow_microphone() -> Result<String> {
    let mut cmd = Command::new(tool("ffmpeg")?);
    cmd.args([
        "-hide_banner",
        "-nostdin",
        "-list_devices",
        "true",
        "-f",
        "dshow",
        "-i",
        "dummy",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    crate::agents::no_console(&mut cmd);

    let output = cmd
        .output()
        .await
        .context("could not ask ffmpeg which microphones Windows has")?;
    // Listing devices always exits non-zero, because there is no real input to
    // open, so the list it prints on stderr is the whole point of the call.
    let listing = String::from_utf8_lossy(&output.stderr);
    pick_dshow_audio(&listing).ok_or_else(|| {
        anyhow!("Windows reported no microphone to record from. Plug one in, or set THREADKNOT_MIC_DEVICE to the device name ffmpeg lists.")
    })
}

/// The best audio device in `ffmpeg -list_devices true -f dshow` output.
///
/// ffmpeg has printed this list two ways over the years: older builds group
/// devices under a "DirectShow audio devices" heading, newer ones tag each
/// entry `(audio)` or `(video)`. Both are read here. The "Alternative name"
/// line that follows a device wins when there is one, because friendly names
/// are not unique and two identical ones make the capture ambiguous.
///
/// Enumeration order is not preference order: machines routinely list a
/// camera's audio input terminal or a virtual device ahead of the actual
/// microphone, and recording those yields perfectly valid silence, which the
/// silence gate then throws away - dictation "works" and hears nothing. So a
/// device that calls itself a microphone beats whatever came first.
#[cfg(any(target_os = "windows", test))]
fn pick_dshow_audio(listing: &str) -> Option<String> {
    let mut audio_section = false;
    // (friendly name, name to capture with) in enumeration order.
    let mut devices: Vec<(String, String)> = Vec::new();
    let mut awaiting_alt = false;

    for raw in listing.lines() {
        let line = trim_log_prefix(raw);
        if line.starts_with("DirectShow video devices") {
            audio_section = false;
            awaiting_alt = false;
        } else if line.starts_with("DirectShow audio devices") {
            audio_section = true;
            awaiting_alt = false;
        } else if let Some(rest) = line.strip_prefix("Alternative name") {
            if awaiting_alt {
                if let (Some(name), Some(last)) = (quoted(rest), devices.last_mut()) {
                    last.1 = name;
                }
                awaiting_alt = false;
            }
        } else if line.starts_with('"') {
            awaiting_alt = false;
            let is_audio = if line.ends_with("(audio)") {
                true
            } else if line.ends_with("(video)") || line.ends_with("(none)") {
                false
            } else {
                audio_section
            };
            if is_audio {
                if let Some(name) = quoted(line) {
                    devices.push((name.clone(), name));
                    awaiting_alt = true;
                }
            }
        }
    }

    devices
        .iter()
        .find(|(friendly, _)| friendly.to_lowercase().contains("microphone"))
        .or_else(|| devices.first())
        .map(|(_, capture)| capture.clone())
}

/// Drop ffmpeg's `[dshow @ 0x...]` log prefix from a line.
#[cfg(any(target_os = "windows", test))]
fn trim_log_prefix(line: &str) -> &str {
    let line = line.trim();
    let stripped = line
        .strip_prefix('[')
        .and_then(|rest| rest.find(']').map(|end| &rest[end + 1..]));
    stripped.unwrap_or(line).trim()
}

/// The text between the outermost double quotes on a line.
#[cfg(any(target_os = "windows", test))]
fn quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let end = line.rfind('"')?;
    (end > start + 1).then(|| line[start + 1..end].to_string())
}

impl Dictation {
    /// Begin capturing. Returns the id `stop`/`cancel` expect.
    pub async fn start(&self) -> Result<String> {
        if let Some(hint) = missing_tool() {
            return Err(anyhow!(hint));
        }
        // Whatever was running is stale the moment a new capture is asked for.
        self.discard_active().await;

        let (format, device) = capture_args().await?;
        let id = crate::protocol::new_id();
        let dir = std::env::temp_dir().join(format!("threadknot-dictation-{id}"));
        std::fs::create_dir_all(&dir).context("could not create the dictation scratch dir")?;
        let wav = dir.join("clip.wav");

        let mut cmd = Command::new(tool("ffmpeg")?);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            format,
            "-i",
            &device,
            "-ac",
            "1",
            "-ar",
            SAMPLE_RATE,
            "-t",
            &MAX_SECONDS.to_string(),
        ])
        .arg(&wav)
        // ffmpeg reads stdin for keyboard commands; `stop` writes "q" there
        // so the WAV header is finalized instead of truncated.
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        crate::agents::no_console(&mut cmd);
        let ffmpeg = cmd
            .spawn()
            .context("could not start ffmpeg to record the microphone")?;

        *self.active.lock().unwrap() = Some(Active {
            id: id.clone(),
            ffmpeg,
            dir,
            wav,
            started: std::time::Instant::now(),
        });
        Ok(id)
    }

    /// Stop capturing and transcribe. Empty text means nothing was said.
    pub async fn stop(&self, id: &str) -> Result<String> {
        let mut active = self
            .take(id)
            .ok_or_else(|| anyhow!("that recording already stopped"))?;
        let elapsed = active.started.elapsed().as_millis();
        let stderr = close_ffmpeg(&mut active.ffmpeg).await;

        let result = async {
            if elapsed < MIN_MILLIS {
                return Ok(String::new());
            }
            if !active.wav.exists() {
                return Err(anyhow!(
                    "the microphone produced no audio{}",
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(" — ffmpeg said: {stderr}")
                    }
                ));
            }
            if is_silent(&active.wav).await {
                return Ok(String::new());
            }
            transcribe(&active.wav, &active.dir).await
        }
        .await;

        let _ = std::fs::remove_dir_all(&active.dir);
        result
    }

    /// Throw the clip away without transcribing it.
    pub async fn cancel(&self, id: &str) {
        if let Some(mut active) = self.take(id) {
            let _ = active.ffmpeg.kill().await;
            let _ = std::fs::remove_dir_all(&active.dir);
        }
    }

    /// Remove the slot's recording if it is the one named.
    fn take(&self, id: &str) -> Option<Active> {
        let mut slot = self.active.lock().unwrap();
        match slot.as_ref() {
            Some(a) if a.id == id => slot.take(),
            _ => None,
        }
    }

    async fn discard_active(&self) {
        let previous = self.active.lock().unwrap().take();
        if let Some(mut a) = previous {
            let _ = a.ffmpeg.kill().await;
            let _ = std::fs::remove_dir_all(&a.dir);
        }
    }
}

/// Ask ffmpeg to finish the file, then wait briefly before killing it. Returns
/// whatever it wrote to stderr, which is the only clue when capture failed.
async fn close_ffmpeg(child: &mut Child) -> String {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"q").await;
        let _ = stdin.flush().await;
    }
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            let _ = child.kill().await;
        }
    }
    let mut text = String::new();
    if let Some(mut err) = child.stderr.take() {
        use tokio::io::AsyncReadExt;
        let _ = err.read_to_string(&mut text).await;
    }
    text.trim().to_string()
}

/// True when the clip carries no signal worth transcribing.
async fn is_silent(wav: &Path) -> bool {
    let Ok(bin) = tool("ffmpeg") else { return false };
    let mut cmd = Command::new(bin);
    cmd.args(["-hide_banner", "-nostdin", "-i"])
        .arg(wav)
        .args(["-af", "volumedetect", "-f", "null", "-"])
        .stdin(Stdio::null());
    crate::agents::no_console(&mut cmd);
    let output = cmd.output().await;
    let Ok(output) = output else { return false };
    let text = String::from_utf8_lossy(&output.stderr);
    let Some(rest) = text.split("mean_volume:").nth(1) else {
        // No reading at all: let Whisper decide rather than drop real audio.
        return false;
    };
    rest.split_whitespace()
        .next()
        .and_then(|v| v.parse::<f32>().ok())
        .map(|db| db < SILENCE_DBFS)
        .unwrap_or(false)
}

/// Run Whisper over the clip and return what it heard.
async fn transcribe(wav: &Path, dir: &Path) -> Result<String> {
    // English-only by default: `base.en` is markedly better than `base` at the
    // same speed. `THREADKNOT_WHISPER_MODEL` swaps in a bigger or multilingual one.
    let model = std::env::var("THREADKNOT_WHISPER_MODEL").unwrap_or_else(|_| "base.en".into());
    let multilingual = !model.ends_with(".en");

    let mut cmd = Command::new(tool("whisper")?);
    cmd.arg(wav)
        .args(["--model", &model])
        .args(["--task", "transcribe"])
        .args(["--output_format", "txt"])
        .arg("--output_dir")
        .arg(dir)
        // Each clip stands alone; carrying context between them is what makes
        // Whisper loop the same phrase over and over.
        .args(["--condition_on_previous_text", "False"])
        .args(["--verbose", "False"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if !multilingual {
        cmd.args(["--language", "en"]);
    }
    crate::agents::no_console(&mut cmd);

    let output = cmd
        .output()
        .await
        .context("could not run whisper to transcribe the recording")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let tail = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(anyhow!("whisper failed: {tail}"));
    }

    let txt = dir.join("clip.txt");
    let text = std::fs::read_to_string(&txt).unwrap_or_default();
    Ok(clean(&text))
}

/// Collapse Whisper's line-per-segment output into one line, and drop the
/// filler it invents when it hears nothing.
fn clean(raw: &str) -> String {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let bare = joined
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if bare.is_empty() || HALLUCINATIONS.contains(&bare.as_str()) {
        return String::new();
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::{clean, pick_dshow_audio};

    /// ffmpeg 7: every device carries its own `(audio)` / `(video)` tag.
    const TAGGED: &str = r#"
[dshow @ 000001d1] "Integrated Webcam" (video)
[dshow @ 000001d1]   Alternative name "@device_pnp_\\?\usb#vid_0c45"
[dshow @ 000001d1] "Microphone Array (Realtek(R) Audio)" (audio)
[dshow @ 000001d1]   Alternative name "@device_cm_{33D9A762}\wave_{B1F2C3}"
[dshow @ 000001d1] "Headset (Jabra)" (audio)
"#;

    /// Older ffmpeg: devices grouped under headings, with no per-device tag.
    const HEADED: &str = r#"
[dshow @ 000001d1] DirectShow video devices (some may be both video and audio devices)
[dshow @ 000001d1]  "Integrated Webcam"
[dshow @ 000001d1]     Alternative name "@device_pnp_\\?\usb#vid_0c45"
[dshow @ 000001d1] DirectShow audio devices
[dshow @ 000001d1]  "Microphone Array (Realtek(R) Audio)"
[dshow @ 000001d1]     Alternative name "@device_cm_{33D9A762}\wave_{B1F2C3}"
"#;

    #[test]
    fn picks_the_first_microphone_from_tagged_listings() {
        assert_eq!(
            pick_dshow_audio(TAGGED).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{B1F2C3}")
        );
    }

    #[test]
    fn picks_the_first_microphone_from_headed_listings() {
        assert_eq!(
            pick_dshow_audio(HEADED).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{B1F2C3}")
        );
    }

    #[test]
    fn falls_back_to_the_friendly_name_when_there_is_no_alternative() {
        let listing = "[dshow @ 1] \"Blue Yeti\" (audio)\n";
        assert_eq!(pick_dshow_audio(listing).as_deref(), Some("Blue Yeti"));
    }

    /// A real laptop's listing: a camera's audio input terminal and a virtual
    /// device enumerate AHEAD of the actual microphone. Recording the terminal
    /// yields valid silence, so picking by order alone makes dictation hear
    /// nothing on machines like this.
    const MIC_LAST: &str = r#"
[in#0 @ 0000024d] "HP True Vision FHD Camera" (video)
[in#0 @ 0000024d]   Alternative name "@device_pnp_\\?\usb#vid_0408"
[in#0 @ 0000024d] "OBS Virtual Camera" (none)
[in#0 @ 0000024d]   Alternative name "@device_sw_{860BB310}\{A3FCE0F5}"
[in#0 @ 0000024d] "Capture Input terminal (2- UAC HD Camera)" (audio)
[in#0 @ 0000024d]   Alternative name "@device_cm_{33D9A762}\wave_{022FF914}"
[in#0 @ 0000024d] "Virtual audio desktop" (audio)
[in#0 @ 0000024d]   Alternative name "@device_sw_{33D9A762}\{8E146464}"
[in#0 @ 0000024d] "Microphone Array (AMD Audio Device)" (audio)
[in#0 @ 0000024d]   Alternative name "@device_cm_{33D9A762}\wave_{C411E5E5}"
"#;

    #[test]
    fn prefers_a_named_microphone_over_earlier_audio_devices() {
        assert_eq!(
            pick_dshow_audio(MIC_LAST).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{C411E5E5}")
        );
    }

    #[test]
    fn keeps_the_first_audio_device_when_nothing_is_named_microphone() {
        let listing = "[dshow @ 1] \"Line In (High Definition Audio)\" (audio)\n\
                       [dshow @ 1] \"Stereo Mix (High Definition Audio)\" (audio)\n";
        assert_eq!(
            pick_dshow_audio(listing).as_deref(),
            Some("Line In (High Definition Audio)")
        );
    }

    #[test]
    fn reports_nothing_when_the_machine_has_no_microphone() {
        let listing = "[dshow @ 1] \"Integrated Webcam\" (video)\n\
                       [dshow @ 1] Could not enumerate audio only devices (or none found).\n";
        assert_eq!(pick_dshow_audio(listing), None);
    }

    #[test]
    fn joins_segments_onto_one_line() {
        assert_eq!(clean(" Open the\n file.\n Then run it.\n"), "Open the file. Then run it.");
    }

    #[test]
    fn drops_silence_hallucinations() {
        assert_eq!(clean("You\n"), "");
        assert_eq!(clean(" Thanks for watching! "), "");
        assert_eq!(clean("  \n"), "");
    }

    #[test]
    fn keeps_real_speech_that_starts_with_a_filler_word() {
        assert_eq!(clean("You should refactor this."), "You should refactor this.");
    }
}
