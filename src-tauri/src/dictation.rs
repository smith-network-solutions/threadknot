//! Voice dictation for the composer.
//!
//! The webview can't do this itself. WebKitGTK ships `enable-media-stream` off
//! and wry installs no `permission-request` handler, so `getUserMedia` is
//! simply unavailable inside the desktop shell — a browser-side recorder would
//! be dead in the app it's meant for. So Threadknot records the machine's own
//! microphone with ffmpeg (already a runtime dependency for the browser
//! recorder).
//!
//! Dictation is live: the mic streams into an utterance endpointer and the
//! words are pushed to the composer while the user is still talking (see
//! `live.rs`). Local transcription goes through the warm faster-whisper sidecar
//! that voice sessions use, falling back to the per-clip `whisper` CLI where
//! the sidecar cannot run. An explicitly configured OpenAI-compatible API is
//! the other provider.
//!
//! There is one microphone, so there is one recording slot. Starting a second
//! recording discards the first rather than fighting over the device.

mod live;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub use live::Outcome;

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

/// A recording in progress: the task running the live loop and the two ways
/// to end it.
struct Active {
    id: String,
    /// Stop: finish the sentence that is open, then return everything heard.
    stop: CancellationToken,
    /// Cancel: throw it all away and say nothing more.
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<Result<Outcome>>,
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
    /// Set the first time dictation is used on this machine. It is what makes
    /// preloading the speech model at launch worth its memory: someone who has
    /// never dictated should not have a model loaded on their behalf.
    #[serde(default)]
    used: bool,
}

impl Default for DictationConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Local,
            base_url: default_api_base_url(),
            model: default_api_model(),
            api_key: String::new(),
            used: false,
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
pub(crate) fn tool(name: &str) -> Result<PathBuf> {
    crate::agents::resolve_bin(name).ok_or_else(|| anyhow!("{name} is not installed"))
}

/// Why this machine can't capture audio, or `None` when it can.
pub(crate) fn missing_capture_tool() -> Option<String> {
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
pub(crate) fn missing_local_transcriber() -> Option<String> {
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
pub(crate) async fn capture_args() -> Result<(&'static str, String)> {
    Ok(("pulse", mic_override().unwrap_or_else(|| "default".into())))
}

/// ffmpeg's capture flags for this platform's default input device.
#[cfg(target_os = "macos")]
pub(crate) async fn capture_args() -> Result<(&'static str, String)> {
    Ok((
        "avfoundation",
        mic_override().unwrap_or_else(|| ":default".into()),
    ))
}

/// ffmpeg's capture flags for this platform's default input device.
///
/// DirectShow has no notion of a default microphone: every capture has to name
/// a real device, and the names are machine-specific. So ask ffmpeg what
/// Windows can see, ask Windows which input it treats as the default, and
/// record that one.
#[cfg(target_os = "windows")]
pub(crate) async fn capture_args() -> Result<(&'static str, String)> {
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
pub(crate) async fn capture_args() -> Result<(&'static str, String)> {
    Err(anyhow!(
        "dictation is only supported on Windows, macOS and Linux"
    ))
}

/// Ask ffmpeg which microphones DirectShow can see and pick the right one.
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
    pick_dshow_audio(&listing, windows_default_input().as_deref()).ok_or_else(|| {
        anyhow!("Windows reported no microphone to record from. Plug one in, or set THREADKNOT_MIC_DEVICE to the device name ffmpeg lists.")
    })
}

/// The name of the recording device Windows itself treats as the default: the
/// one picked in Sound settings, and the one every other app records from.
///
/// This is the classic wave mapper query. It needs no COM and no extra crate
/// features, at the price of one quirk: the wave API truncates device names to
/// 31 characters, so the result is a prefix of the name ffmpeg lists.
#[cfg(target_os = "windows")]
fn windows_default_input() -> Option<String> {
    #[repr(C)]
    struct WaveInCapsW {
        mid: u16,
        pid: u16,
        driver_version: u32,
        name: [u16; 32],
        formats: u32,
        channels: u16,
        reserved: u16,
    }
    #[link(name = "winmm")]
    extern "system" {
        fn waveInMessage(device: isize, message: u32, param1: usize, param2: usize) -> u32;
        fn waveInGetDevCapsW(device: usize, caps: *mut WaveInCapsW, size: u32) -> u32;
    }
    const WAVE_MAPPER: isize = -1;
    const DRVM_MAPPER_PREFERRED_GET: u32 = 0x2015;

    let mut id: u32 = u32::MAX;
    let mut flags: u32 = 0;
    // SAFETY: both out-parameters point at live u32s for the whole call, which
    // is what DRVM_MAPPER_PREFERRED_GET writes through.
    let rc = unsafe {
        waveInMessage(
            WAVE_MAPPER,
            DRVM_MAPPER_PREFERRED_GET,
            &mut id as *mut u32 as usize,
            &mut flags as *mut u32 as usize,
        )
    };
    if rc != 0 || id == u32::MAX {
        return None;
    }
    // SAFETY: an all-zero WaveInCapsW is a valid value, and the size passed is
    // the size of the buffer the pointer names.
    let mut caps: WaveInCapsW = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        waveInGetDevCapsW(
            id as usize,
            &mut caps,
            std::mem::size_of::<WaveInCapsW>() as u32,
        )
    };
    if rc != 0 {
        return None;
    }
    let len = caps.name.iter().position(|&c| c == 0).unwrap_or(caps.name.len());
    let name = String::from_utf16_lossy(&caps.name[..len]).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The best audio device in `ffmpeg -list_devices true -f dshow` output.
///
/// ffmpeg has printed this list two ways over the years: older builds group
/// devices under a "DirectShow audio devices" heading, newer ones tag each
/// entry `(audio)` or `(video)`. Both are read here. The "Alternative name"
/// line that follows a device wins when there is one, because friendly names
/// are not unique and two identical ones make the capture ambiguous.
///
/// Three rules, strongest first:
///
/// 1. The device Windows calls the default (`preferred`, a prefix of its name)
///    wins. It is what the user chose and what every other app records from. A
///    name is not evidence: an interface input can be called "Video Call L + R"
///    while a muted podcast mic beside it is called "Microphone", and choosing
///    by name records the muted one and hears nothing.
/// 2. Without that, a device that calls itself a microphone beats whatever
///    came first. Enumeration order is not preference order: machines list a
///    camera's audio input terminal or a virtual device ahead of the real mic.
/// 3. Otherwise the first audio device.
#[cfg(any(target_os = "windows", test))]
fn pick_dshow_audio(listing: &str, preferred: Option<&str>) -> Option<String> {
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

    let preferred = preferred
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty());
    preferred
        .and_then(|p| {
            devices
                .iter()
                .find(|(friendly, _)| friendly.to_lowercase().starts_with(&p))
        })
        .or_else(|| {
            devices
                .iter()
                .find(|(friendly, _)| friendly.to_lowercase().contains("microphone"))
        })
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

    /// Should the speech model be loaded before anyone asks for it?
    ///
    /// Yes when this machine dictates locally and has done so before. The load
    /// takes several seconds, and paid at the first click it lands exactly
    /// when the user starts talking: they finish a sentence, nothing has
    /// appeared, and live dictation looks broken.
    pub fn wants_warm_model(&self) -> bool {
        let config = self.config.lock().unwrap();
        config.used && config.provider == Provider::Local && missing_capture_tool().is_none()
    }

    /// Remember that dictation is used here, for `wants_warm_model`.
    fn mark_used(&self) {
        let next = {
            let config = self.config.lock().unwrap();
            if config.used {
                return;
            }
            let mut next = config.clone();
            next.used = true;
            next
        };
        if let Err(e) = self.flush(&next) {
            tracing::warn!("could not record that dictation is in use: {e:#}");
            return;
        }
        *self.config.lock().unwrap() = next;
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

    /// Begin capturing. Returns the id `stop`/`cancel` expect. From here on
    /// the words arrive as `dictation.live` broadcasts while they are spoken.
    pub async fn start(
        &self,
        voice: Arc<crate::voice::Voice>,
        hub: Arc<crate::agents::Hub>,
    ) -> Result<String> {
        if let Some(hint) = self.unavailable_reason() {
            return Err(anyhow!(hint));
        }
        // Whatever was running is stale the moment a new capture is asked for.
        self.discard_active();

        let id = crate::protocol::new_id();
        let stop = CancellationToken::new();
        let cancel = CancellationToken::new();
        // Open the mic before answering, so "no microphone" is an error on the
        // button and not a recording that silently hears nothing.
        let (ffmpeg, mic) = crate::voice::session::spawn_mic(&cancel).await?;
        self.mark_used();
        let config = self.config.lock().unwrap().clone();
        let task = tokio::spawn(live::run(
            id.clone(),
            voice,
            hub,
            config,
            ffmpeg,
            mic,
            stop.clone(),
            cancel.clone(),
        ));

        *self.active.lock().unwrap() = Some(Active {
            id: id.clone(),
            stop,
            cancel,
            task,
        });
        Ok(id)
    }

    /// Stop capturing, finish the sentence that was open, and return all of
    /// it. Empty text means nothing was said.
    pub async fn stop(&self, id: &str) -> Result<Outcome> {
        let active = self
            .take(id)
            .ok_or_else(|| anyhow!("that recording already stopped"))?;
        active.stop.cancel();
        active
            .task
            .await
            .map_err(|e| anyhow!("dictation stopped unexpectedly: {e}"))?
    }

    /// Throw the recording away without transcribing what is left of it.
    pub async fn cancel(&self, id: &str) {
        if let Some(active) = self.take(id) {
            active.cancel.cancel();
        }
    }

    /// Is the microphone being captured right now? Voice sessions refuse to
    /// start while it is (and vice versa): there is one microphone. A session
    /// that ran into its time limit has already let go of the mic, so it does
    /// not count even though nobody has collected its text yet.
    pub fn is_recording(&self) -> bool {
        self.active
            .lock()
            .unwrap()
            .as_ref()
            .map(|a| !a.task.is_finished())
            .unwrap_or(false)
    }

    /// Remove the slot's recording if it is the one named.
    fn take(&self, id: &str) -> Option<Active> {
        let mut slot = self.active.lock().unwrap();
        match slot.as_ref() {
            Some(a) if a.id == id => slot.take(),
            _ => None,
        }
    }

    fn discard_active(&self) {
        if let Some(a) = self.active.lock().unwrap().take() {
            a.cancel.cancel();
        }
    }
}

/// Run Whisper over the clip and return what it heard.
pub(crate) async fn transcribe_local(wav: &Path, dir: &Path) -> Result<String> {
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
pub(crate) fn clean(raw: &str) -> String {
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
            pick_dshow_audio(TAGGED, None).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{B1F2C3}")
        );
    }

    #[test]
    fn picks_the_first_microphone_from_headed_listings() {
        assert_eq!(
            pick_dshow_audio(HEADED, None).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{B1F2C3}")
        );
    }

    #[test]
    fn falls_back_to_the_friendly_name_when_there_is_no_alternative() {
        let listing = "[dshow @ 1] \"Blue Yeti\" (audio)\n";
        assert_eq!(pick_dshow_audio(listing, None).as_deref(), Some("Blue Yeti"));
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
            pick_dshow_audio(MIC_LAST, None).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{C411E5E5}")
        );
    }

    /// A real desk: the working input is an audio interface whose name says
    /// nothing about microphones, and a muted USB podcast mic sits beside it.
    /// Windows' default is the interface; choosing by name records silence.
    const INTERFACE_DEFAULT: &str = r#"
[in#0 @ 0000024d] "Video Call L + R (Focusrite USB Audio)" (audio)
[in#0 @ 0000024d]   Alternative name "@device_cm_{33D9A762}\wave_{EEAF3DAF}"
[in#0 @ 0000024d] "Microphone (PD200X Podcast Microphone)" (audio)
[in#0 @ 0000024d]   Alternative name "@device_cm_{33D9A762}\wave_{15C15843}"
"#;

    #[test]
    fn the_windows_default_input_beats_a_device_named_microphone() {
        // The wave API truncates names to 31 characters, hence the prefix.
        assert_eq!(
            pick_dshow_audio(INTERFACE_DEFAULT, Some("Video Call L + R (Focusrite USB")).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{EEAF3DAF}")
        );
        assert_eq!(
            pick_dshow_audio(INTERFACE_DEFAULT, Some("microphone (pd200x podcast micr")).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{15C15843}")
        );
    }

    #[test]
    fn an_unknown_default_falls_back_to_the_named_microphone() {
        assert_eq!(
            pick_dshow_audio(INTERFACE_DEFAULT, Some("Some Unplugged Headset")).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{15C15843}")
        );
        assert_eq!(
            pick_dshow_audio(INTERFACE_DEFAULT, Some("  ")).as_deref(),
            Some(r"@device_cm_{33D9A762}\wave_{15C15843}")
        );
    }

    #[test]
    fn keeps_the_first_audio_device_when_nothing_is_named_microphone() {
        let listing = "[dshow @ 1] \"Line In (High Definition Audio)\" (audio)\n\
                       [dshow @ 1] \"Stereo Mix (High Definition Audio)\" (audio)\n";
        assert_eq!(
            pick_dshow_audio(listing, None).as_deref(),
            Some("Line In (High Definition Audio)")
        );
    }

    #[test]
    fn reports_nothing_when_the_machine_has_no_microphone() {
        let listing = "[dshow @ 1] \"Integrated Webcam\" (video)\n\
                       [dshow @ 1] Could not enumerate audio only devices (or none found).\n";
        assert_eq!(pick_dshow_audio(listing, None), None);
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
                used: false,
            }),
        };
        let public = dictation.settings();
        assert_eq!(public["hasApiKey"], true);
        assert!(!public.to_string().contains("super-secret-key"));
    }
}
