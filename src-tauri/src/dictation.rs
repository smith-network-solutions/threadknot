//! Voice dictation for the composer.
//!
//! The webview can't do this itself. WebKitGTK ships `enable-media-stream` off
//! and wry installs no `permission-request` handler, so `getUserMedia` is
//! simply unavailable inside the desktop shell — a browser-side recorder would
//! be dead in the app it's meant for. So Threadknot records the machine's own
//! microphone with ffmpeg (already a runtime dependency for the browser
//! recorder). The finished clip is transcribed either by a local Whisper
//! install or by an explicitly configured OpenAI-compatible transcription API.
//!
//! There is one microphone, so there is one recording slot. Starting a second
//! recording discards the first rather than fighting over the device.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
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

const DEFAULT_API_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_API_MODEL: &str = "gpt-transcribe";

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Provider {
    #[default]
    Local,
    Api,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DictationConfig {
    #[serde(default)]
    provider: Provider,
    #[serde(default = "default_api_base_url")]
    base_url: String,
    #[serde(default = "default_api_model")]
    model: String,
    /// Write-only on the wire; persisted with the same local trust boundary as
    /// server.json and never included in settings/hello responses.
    #[serde(default)]
    api_key: String,
}

impl Default for DictationConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Local,
            base_url: default_api_base_url(),
            model: default_api_model(),
            api_key: String::new(),
        }
    }
}

fn default_api_base_url() -> String {
    DEFAULT_API_BASE_URL.into()
}

fn default_api_model() -> String {
    DEFAULT_API_MODEL.into()
}

pub struct Dictation {
    /// One mic, one slot.
    active: Mutex<Option<Active>>,
    path: PathBuf,
    config: Mutex<DictationConfig>,
}

/// Absolute path of a tool we shell out to. Resolved against the agent PATH
/// because a desktop launch inherits almost none of the user's shell PATH, and
/// because on Windows that resolution is what appends `.exe`.
fn tool(name: &str) -> Result<PathBuf> {
    crate::agents::resolve_bin(name).ok_or_else(|| anyhow!("{name} is not installed"))
}

/// Why this machine can't capture audio, or `None` when it can.
fn missing_capture_tool() -> Option<String> {
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
    None
}

/// Why the on-device transcription path can't run, or `None` when it can.
fn missing_local_transcriber() -> Option<String> {
    if tool("whisper").is_err() {
        return Some(
            "Whisper is not installed — run `pip install -U openai-whisper` to dictate".into(),
        );
    }
    None
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

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("dictation.json");
        let config = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?)
                .context("parse dictation.json")?
        } else {
            DictationConfig::default()
        };
        Ok(Self {
            active: Mutex::new(None),
            path,
            config: Mutex::new(config),
        })
    }

    fn unavailable_reason(&self) -> Option<String> {
        if let Some(hint) = missing_capture_tool() {
            return Some(hint);
        }
        let config = self.config.lock().unwrap();
        match config.provider {
            Provider::Local => missing_local_transcriber(),
            Provider::Api if config.api_key.trim().is_empty() => {
                Some("Add a transcription API key in Settings → Voice".into())
            }
            Provider::Api if config.base_url.trim().is_empty() => {
                Some("Add a transcription API base URL in Settings → Voice".into())
            }
            Provider::Api if config.model.trim().is_empty() => {
                Some("Choose a transcription API model in Settings → Voice".into())
            }
            Provider::Api => None,
        }
    }

    /// `{ available, hint }` for hello. Paired clients never get to activate
    /// this machine's microphone, even when a remote transcriber is selected.
    pub fn capability(&self, master: bool) -> serde_json::Value {
        if !master {
            return serde_json::json!({
                "available": false,
                "hint": "Dictation records this machine's mic, so it only runs from the app on that machine",
            });
        }
        match self.unavailable_reason() {
            None => serde_json::json!({ "available": true }),
            Some(hint) => serde_json::json!({ "available": false, "hint": hint }),
        }
    }

    /// Public, secret-free settings for the Voice screen.
    pub fn settings(&self) -> serde_json::Value {
        let config = self.config.lock().unwrap().clone();
        let capture_hint = missing_capture_tool();
        let local_hint = capture_hint.clone().or_else(missing_local_transcriber);
        serde_json::json!({
            "provider": config.provider,
            "baseUrl": config.base_url,
            "model": config.model,
            "hasApiKey": !config.api_key.is_empty(),
            "captureAvailable": capture_hint.is_none(),
            "captureHint": capture_hint,
            "localAvailable": local_hint.is_none(),
            "localHint": local_hint,
        })
    }

    /// Persist a provider choice. `api_key: None` preserves the write-only key;
    /// an explicit empty string clears it.
    pub fn configure(
        &self,
        provider: &str,
        base_url: &str,
        model: &str,
        api_key: Option<&str>,
    ) -> Result<serde_json::Value> {
        let provider = match provider {
            "local" => Provider::Local,
            "api" => Provider::Api,
            _ => anyhow::bail!("unknown dictation provider"),
        };
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        let model = model.trim().to_string();
        let parsed = url::Url::parse(&base_url).context("invalid transcription API base URL")?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "transcription API URL must be an http(s) base URL without credentials, a query, or a fragment"
        );
        anyhow::ensure!(!model.is_empty(), "missing transcription model");

        let mut next = self.config.lock().unwrap().clone();
        next.provider = provider;
        next.base_url = base_url;
        next.model = model;
        if let Some(key) = api_key {
            next.api_key = key.trim().to_string();
        }
        if provider == Provider::Api {
            anyhow::ensure!(!next.api_key.is_empty(), "missing transcription API key");
        }
        self.flush(&next)?;
        *self.config.lock().unwrap() = next;
        Ok(self.settings())
    }

    fn flush(&self, config: &DictationConfig) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(config)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Begin capturing. Returns the id `stop`/`cancel` expect.
    pub async fn start(&self) -> Result<String> {
        if let Some(hint) = self.unavailable_reason() {
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

        let config = self.config.lock().unwrap().clone();
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
            match config.provider {
                Provider::Local => transcribe_local(&active.wav, &active.dir).await,
                Provider::Api => transcribe_api(&active.wav, &config).await,
            }
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
    let Ok(bin) = tool("ffmpeg") else {
        return false;
    };
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
async fn transcribe_local(wav: &Path, dir: &Path) -> Result<String> {
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
        let tail = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(anyhow!("whisper failed: {tail}"));
    }

    let txt = dir.join("clip.txt");
    let text = std::fs::read_to_string(&txt).unwrap_or_default();
    Ok(clean(&text))
}

/// Upload a completed clip to an OpenAI-compatible transcription endpoint.
async fn transcribe_api(wav: &Path, config: &DictationConfig) -> Result<String> {
    const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
    let audio = tokio::fs::read(wav)
        .await
        .context("could not read the recorded audio")?;
    let part = reqwest::multipart::Part::bytes(audio)
        .file_name("clip.wav")
        .mime_str("audio/wav")?;
    let form = reqwest::multipart::Form::new()
        .text("model", config.model.clone())
        .part("file", part);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = client
        .post(format!(
            "{}/audio/transcriptions",
            config.base_url.trim_end_matches('/')
        ))
        .bearer_auth(&config.api_key)
        .multipart(form)
        .send()
        .await
        .context("could not reach the transcription API")?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("could not read the transcription response")?;
    anyhow::ensure!(
        body.len() <= MAX_RESPONSE_BYTES,
        "transcription API response was too large"
    );
    let body = String::from_utf8_lossy(&body);
    if !status.is_success() {
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.pointer("/error/message")?.as_str().map(str::to_string))
            .unwrap_or_else(|| body.chars().take(300).collect());
        anyhow::bail!("transcription API returned {status}: {detail}");
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).context("transcription API returned invalid JSON")?;
    let text = value
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("transcription API response had no text"))?;
    Ok(clean(text))
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
    use super::{clean, pick_dshow_audio, Dictation, DictationConfig, Provider};
    use std::path::PathBuf;
    use std::sync::Mutex;

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
        assert_eq!(
            clean(" Open the\n file.\n Then run it.\n"),
            "Open the file. Then run it."
        );
    }

    #[test]
    fn drops_silence_hallucinations() {
        assert_eq!(clean("You\n"), "");
        assert_eq!(clean(" Thanks for watching! "), "");
        assert_eq!(clean("  \n"), "");
    }

    #[test]
    fn keeps_real_speech_that_starts_with_a_filler_word() {
        assert_eq!(
            clean("You should refactor this."),
            "You should refactor this."
        );
    }

    #[test]
    fn public_settings_never_expose_the_api_key() {
        let dictation = Dictation {
            active: Mutex::new(None),
            path: PathBuf::new(),
            config: Mutex::new(DictationConfig {
                provider: Provider::Api,
                base_url: "https://speech.example/v1".into(),
                model: "whisper-1".into(),
                api_key: "super-secret-key".into(),
            }),
        };
        let public = dictation.settings();
        assert_eq!(public["hasApiKey"], true);
        assert!(!public.to_string().contains("super-secret-key"));
    }
}
