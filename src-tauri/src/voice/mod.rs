//! Voice Parlay: a spoken conversation with whichever agent a thread uses.
//!
//! The pipeline is mic → local Whisper → the thread's agent → ElevenLabs
//! streaming TTS → this machine's speakers. Like dictation, every stage runs
//! server-side: the webview has no `getUserMedia` (see `dictation.rs`), the
//! CSP keeps the frontend off external origins, and the machine that owns the
//! microphone also owns the speakers — so the frontend is a state display with
//! controls, driven by `voice.state` broadcasts.
//!
//! The ElevenLabs API key lives here with the same trust boundary as every
//! other secret file (`server.json`, `dictation.json`, `hermes.json`): written
//! 0600 where the OS honors it, write-only on the wire, and only ever a
//! `hasApiKey` boolean in responses.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub mod chunker;
pub mod eleven;
pub mod playback;
pub mod session;
pub mod stt;

/// How much the agent is asked to say out loud.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    #[default]
    Concise,
    Normal,
    Detailed,
}

/// Per-request ElevenLabs voice tuning. Every field optional: absent means
/// "use the voice's own defaults", and the settings UI only offers the knobs
/// the selected model supports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceTuning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity_boost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_speaker_boost: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

/// Flash is ElevenLabs' low-latency family — conversation cares about time to
/// first audio, not studio narration quality. Not hard-coded anywhere else:
/// the models list is fetched live and the user can pick any TTS model.
pub const DEFAULT_MODEL_ID: &str = "eleven_flash_v2_5";

/// Raw PCM means no decode step between the TTS stream and the audio sink.
pub const DEFAULT_OUTPUT_FORMAT: &str = "pcm_22050";

const DEFAULT_END_SILENCE_MS: u64 = 700;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConfig {
    /// Write-only on the wire; persisted with the same local trust boundary as
    /// server.json and never included in settings/hello responses.
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub voice_id: String,
    /// Display name captured at selection time so the settings screen can show
    /// the choice without a voices fetch.
    #[serde(default)]
    pub voice_name: String,
    #[serde(default = "default_model_id")]
    pub model_id: String,
    #[serde(default = "default_output_format")]
    pub output_format: String,
    #[serde(default)]
    pub voice_settings: VoiceTuning,
    #[serde(default)]
    pub verbosity: Verbosity,
    /// Speaking over the assistant interrupts it.
    #[serde(default = "default_true")]
    pub barge_in: bool,
    /// Silence that ends an utterance.
    #[serde(default = "default_end_silence_ms")]
    pub end_silence_ms: u64,
    /// Return to listening after the assistant finishes speaking.
    #[serde(default = "default_true")]
    pub auto_listen: bool,
}

fn default_true() -> bool {
    true
}
fn default_model_id() -> String {
    DEFAULT_MODEL_ID.into()
}
fn default_output_format() -> String {
    DEFAULT_OUTPUT_FORMAT.into()
}
fn default_end_silence_ms() -> u64 {
    DEFAULT_END_SILENCE_MS
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            enabled: true,
            voice_id: String::new(),
            voice_name: String::new(),
            model_id: default_model_id(),
            output_format: default_output_format(),
            voice_settings: VoiceTuning::default(),
            verbosity: Verbosity::default(),
            barge_in: true,
            end_silence_ms: DEFAULT_END_SILENCE_MS,
            auto_listen: true,
        }
    }
}

/// Partial update from `voice.settings.save`. Absent fields keep their current
/// value; `api_key` follows dictation's write-only rule (absent preserves, an
/// explicit empty string clears).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConfigInput {
    pub api_key: Option<String>,
    pub enabled: Option<bool>,
    pub voice_id: Option<String>,
    pub voice_name: Option<String>,
    pub model_id: Option<String>,
    pub output_format: Option<String>,
    pub voice_settings: Option<VoiceTuning>,
    pub verbosity: Option<Verbosity>,
    pub barge_in: Option<bool>,
    pub end_silence_ms: Option<u64>,
    pub auto_listen: Option<bool>,
}

pub struct Voice {
    path: PathBuf,
    config: Mutex<VoiceConfig>,
    /// The running voice preview, if any. One pair of speakers: starting a
    /// new preview (or stopping) cancels the old one.
    preview: Mutex<Option<tokio_util::sync::CancellationToken>>,
    /// The live conversation, if any. One microphone: one session slot.
    session: Mutex<Option<session::SessionCtl>>,
    /// The warm STT sidecar, shared across sessions.
    pub(crate) stt: tokio::sync::Mutex<session::SttState>,
    /// Monotonic counter on `voice.state` frames so clients drop stale ones.
    revision: std::sync::atomic::AtomicU64,
}

impl Voice {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("voice.json");
        let config = if path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&path)?).context("parse voice.json")?
        } else {
            VoiceConfig::default()
        };
        Ok(Self {
            path,
            config: Mutex::new(config),
            preview: Mutex::new(None),
            session: Mutex::new(None),
            stt: tokio::sync::Mutex::new(session::SttState::default()),
            revision: std::sync::atomic::AtomicU64::new(0),
        })
    }

    // ---- session slot --------------------------------------------------------

    pub fn session_active(&self) -> bool {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| !s.cancel.is_cancelled())
            .unwrap_or(false)
    }

    fn set_session(&self, ctl: session::SessionCtl) {
        *self.session.lock().unwrap() = Some(ctl);
    }

    /// Cancel whatever session is running (idempotent).
    pub fn end_session(&self) {
        if let Some(ctl) = self.session.lock().unwrap().take() {
            ctl.cancel.cancel();
        }
    }

    /// Drop the slot only if it still belongs to the session that ended.
    /// Returns whether it did — a replaced session must NOT go on to publish
    /// idle over the state of the session that replaced it.
    fn clear_session(&self, session_id: &str) -> bool {
        let mut slot = self.session.lock().unwrap();
        if slot.as_ref().map(|s| s.id.as_str()) == Some(session_id) {
            *slot = None;
            return true;
        }
        false
    }

    /// Run `f` against the named live session's controls.
    fn with_session<T>(
        &self,
        session_id: &str,
        f: impl FnOnce(&session::SessionCtl) -> T,
    ) -> Result<T> {
        let slot = self.session.lock().unwrap();
        match slot.as_ref() {
            Some(ctl) if ctl.id == session_id => Ok(f(ctl)),
            _ => anyhow::bail!("that voice session already ended"),
        }
    }

    // ---- state broadcast -----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_frame(
        &self,
        hub: &crate::agents::Hub,
        session_id: Option<&str>,
        thread_id: Option<&str>,
        state: &str,
        muted: bool,
        last_utterance: Option<&str>,
        detail: Option<&str>,
        error: Option<crate::protocol::VoiceError>,
        mic_level: Option<f32>,
    ) {
        use std::sync::atomic::Ordering;
        let frame = crate::protocol::VoiceStateFrame {
            session_id: session_id.map(str::to_string),
            thread_id: thread_id.map(str::to_string),
            state: state.to_string(),
            muted,
            last_utterance: last_utterance.map(str::to_string),
            detail: detail.map(str::to_string),
            error,
            mic_level,
            since: crate::protocol::now_iso(),
            revision: self.revision.fetch_add(1, Ordering::Relaxed) + 1,
        };
        let _ = hub
            .broadcast
            .send(crate::protocol::ServerMessage::VoiceState { state: frame });
    }

    pub(crate) fn publish_idle(&self, hub: &crate::agents::Hub) {
        self.publish_frame(hub, None, None, "idle", false, None, None, None, None);
    }

    // ---- STT sidecar lifecycle ----------------------------------------------

    /// Kill the sidecar if nothing has used it for `idle`.
    pub(crate) async fn reap_idle_sidecar(&self, idle: std::time::Duration) {
        if self.session_active() {
            return;
        }
        let mut state = self.stt.lock().await;
        let stale = state
            .idle_since()
            .map(|t| t.elapsed() >= idle)
            .unwrap_or(false);
        if stale {
            if let Some(sidecar) = state.take_sidecar() {
                sidecar.kill().await;
            }
        }
    }

    // ---- local usage metrics -------------------------------------------------

    /// Append one TTS request to voice-usage.jsonl. Estimates only — the
    /// subscription meter is the source of truth. Never the key.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_tts_metric(
        &self,
        session_id: &str,
        thread_id: &str,
        voice_id: &str,
        model_id: &str,
        chars: u64,
        ttfb_ms: u64,
        total_ms: u64,
    ) {
        let record = serde_json::json!({
            "ts": crate::protocol::now_iso(),
            "sessionId": session_id,
            "threadId": thread_id,
            "provider": "elevenlabs",
            "voiceId": voice_id,
            "modelId": model_id,
            "chars": chars,
            // 1 character ≈ 1 credit is the baseline; model multipliers vary,
            // which is why every surface labels this an estimate.
            "estCredits": chars,
            "ttfbMs": ttfb_ms,
            "streamMs": total_ms,
        });
        let path = self.path.with_file_name("voice-usage.jsonl");
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{record}");
        }
    }

    /// Cancel whatever preview is playing; returns a fresh token when the
    /// caller is starting a new one.
    fn replace_preview(&self, next: Option<tokio_util::sync::CancellationToken>) {
        let mut slot = self.preview.lock().unwrap();
        if let Some(old) = slot.take() {
            old.cancel();
        }
        *slot = next;
    }

    pub fn config(&self) -> VoiceConfig {
        self.config.lock().unwrap().clone()
    }

    pub fn has_api_key(&self) -> bool {
        !self.config.lock().unwrap().api_key.trim().is_empty()
    }

    pub fn api_key(&self) -> Option<String> {
        let key = self.config.lock().unwrap().api_key.trim().to_string();
        (!key.is_empty()).then_some(key)
    }

    /// Why this machine can't hold a voice conversation, or `None` when it can.
    fn unavailable_reason(&self) -> Option<String> {
        if let Some(hint) = crate::dictation::missing_capture_tool() {
            return Some(hint);
        }
        if crate::agents::resolve_bin("ffplay").is_none() {
            return Some(
                "ffplay is not installed — voice replies play through it (it ships with the full ffmpeg build)".into(),
            );
        }
        if crate::agents::resolve_bin("python").is_none()
            && crate::agents::resolve_bin("python3").is_none()
            && crate::dictation::missing_local_transcriber().is_some()
        {
            return Some(
                "No speech-to-text found — install Python with faster-whisper, or `pip install -U openai-whisper`".into(),
            );
        }
        None
    }

    /// Best-guess transcription path, for the settings screen. The sidecar can
    /// still fall back to the whisper CLI at runtime if faster-whisper turns
    /// out not to be importable.
    fn stt_mode(&self) -> (&'static str, Option<String>) {
        let python = crate::agents::resolve_bin("python")
            .or_else(|| crate::agents::resolve_bin("python3"))
            .is_some();
        let cli = crate::dictation::missing_local_transcriber().is_none();
        match (python, cli) {
            (true, _) => ("sidecar", None),
            (false, true) => (
                "cli",
                Some("Python not found — using the slower whisper CLI per utterance".into()),
            ),
            (false, false) => (
                "unavailable",
                Some("Install Python with faster-whisper, or `pip install -U openai-whisper`".into()),
            ),
        }
    }

    /// `{ available, configured, hint }` for hello. Voice parlay uses this
    /// machine's mic and speakers, so it only runs from the app on that
    /// machine — paired devices and peers never see it as available.
    pub fn capability(&self, master: bool) -> serde_json::Value {
        if !master {
            return serde_json::json!({
                "available": false,
                "configured": false,
                "hint": "Voice conversations use this machine's mic and speakers, so they only run from the app on that machine",
            });
        }
        let configured = {
            let config = self.config.lock().unwrap();
            config.enabled && !config.api_key.trim().is_empty()
        };
        match self.unavailable_reason() {
            None => serde_json::json!({ "available": true, "configured": configured }),
            Some(hint) => serde_json::json!({
                "available": false,
                "configured": configured,
                "hint": hint,
            }),
        }
    }

    /// Public, secret-free settings for the Voice screen. The key itself never
    /// leaves this machine — only its presence and a two-character tail so the
    /// UI can render a masked "sk_••••42"-style reminder.
    pub fn settings(&self) -> serde_json::Value {
        let config = self.config.lock().unwrap().clone();
        let capture_hint = crate::dictation::missing_capture_tool();
        let playback_hint = crate::agents::resolve_bin("ffplay").is_none().then(|| {
            "ffplay is not installed — it ships with the full ffmpeg build".to_string()
        });
        let (stt_mode, stt_hint) = self.stt_mode();
        serde_json::json!({
            "hasApiKey": !config.api_key.trim().is_empty(),
            "keyHint": key_hint(&config.api_key),
            "enabled": config.enabled,
            "voiceId": config.voice_id,
            "voiceName": config.voice_name,
            "modelId": config.model_id,
            "outputFormat": config.output_format,
            "voiceSettings": config.voice_settings,
            "verbosity": config.verbosity,
            "bargeIn": config.barge_in,
            "endSilenceMs": config.end_silence_ms,
            "autoListen": config.auto_listen,
            "captureAvailable": capture_hint.is_none(),
            "captureHint": capture_hint,
            "playbackAvailable": playback_hint.is_none(),
            "playbackHint": playback_hint,
            "sttMode": stt_mode,
            "sttHint": stt_hint,
        })
    }

    /// Apply a partial update and persist. Returns the new public settings.
    pub fn configure(&self, input: VoiceConfigInput) -> Result<serde_json::Value> {
        let mut next = self.config.lock().unwrap().clone();
        if let Some(key) = input.api_key {
            next.api_key = key.trim().to_string();
        }
        if let Some(enabled) = input.enabled {
            next.enabled = enabled;
        }
        if let Some(voice_id) = input.voice_id {
            next.voice_id = voice_id.trim().to_string();
        }
        if let Some(voice_name) = input.voice_name {
            next.voice_name = voice_name.trim().to_string();
        }
        if let Some(model_id) = input.model_id {
            let model_id = model_id.trim().to_string();
            anyhow::ensure!(!model_id.is_empty(), "missing TTS model id");
            next.model_id = model_id;
        }
        if let Some(format) = input.output_format {
            let format = format.trim().to_string();
            anyhow::ensure!(
                format
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "unrecognized output format"
            );
            next.output_format = format;
        }
        if let Some(tuning) = input.voice_settings {
            for (name, value) in [
                ("stability", tuning.stability),
                ("similarity boost", tuning.similarity_boost),
                ("style", tuning.style),
            ] {
                if let Some(v) = value {
                    anyhow::ensure!((0.0..=1.0).contains(&v), "{name} must be between 0 and 1");
                }
            }
            if let Some(speed) = tuning.speed {
                anyhow::ensure!(
                    (0.5..=1.5).contains(&speed),
                    "speed must be between 0.5 and 1.5"
                );
            }
            next.voice_settings = tuning;
        }
        if let Some(verbosity) = input.verbosity {
            next.verbosity = verbosity;
        }
        if let Some(barge_in) = input.barge_in {
            next.barge_in = barge_in;
        }
        if let Some(ms) = input.end_silence_ms {
            anyhow::ensure!(
                (200..=5000).contains(&ms),
                "endSilenceMs must be between 200 and 5000"
            );
            next.end_silence_ms = ms;
        }
        if let Some(auto_listen) = input.auto_listen {
            next.auto_listen = auto_listen;
        }

        self.flush(&next)?;
        *self.config.lock().unwrap() = next;
        Ok(self.settings())
    }

    fn flush(&self, config: &VoiceConfig) -> Result<()> {
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
}

/// Dispatch for every `voice.*` request. The caller (server.rs) has already
/// established the principal is the machine owner.
pub async fn handle(
    state: &crate::server::ServerState,
    kind: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value> {
    let require_key = || {
        state.voice.api_key().ok_or_else(|| {
            anyhow::anyhow!("No ElevenLabs API key saved — add one in Settings → Voice")
        })
    };
    match kind {
        "voice.settings.get" => Ok(state.voice.settings()),
        "voice.settings.save" => {
            let input: VoiceConfigInput = serde_json::from_value(payload.clone())
                .context("could not read the voice settings payload")?;
            let settings = state.voice.configure(input)?;
            // The composer's voice button gates on hello.voice.configured;
            // nudge open clients to re-request hello. A key change also makes
            // the ElevenLabs usage row appear/disappear.
            state.hub.broadcast_state("identity", None);
            state.hub.usage.kick(true);
            Ok(settings)
        }
        // Live account state — doubles as key validation.
        "voice.test" => {
            let subscription = eleven::subscription(&require_key()?).await?;
            Ok(serde_json::json!({ "ok": true, "subscription": subscription }))
        }
        "voice.models.list" => eleven::models(&require_key()?).await,
        "voice.voices.search" => {
            let str_field = |name: &str| payload.get(name).and_then(serde_json::Value::as_str);
            eleven::voices_search(
                &require_key()?,
                str_field("search"),
                str_field("category"),
                str_field("pageToken"),
            )
            .await
        }
        "voice.voice.get" => {
            let voice_id = payload
                .get("voiceId")
                .and_then(serde_json::Value::as_str)
                .context("missing voiceId")?;
            eleven::voice(&require_key()?, voice_id).await
        }
        "voice.voiceSettings.default" => eleven::default_voice_settings(&require_key()?).await,
        // Play a voice sample through this machine's speakers. A supplied
        // previewUrl (from the voices list) costs nothing; without one the
        // sample is synthesized, which spends credits — the UI says so.
        "voice.preview" => {
            let preview_url = payload
                .get("previewUrl")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let voice_id = payload
                .get("voiceId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);

            let config = state.voice.config();
            let (response, pcm_rate) = match &preview_url {
                Some(url) => {
                    anyhow::ensure!(
                        url.starts_with("https://"),
                        "preview URLs must be https"
                    );
                    let response = reqwest::Client::builder()
                        .connect_timeout(std::time::Duration::from_secs(8))
                        .build()?
                        .get(url)
                        .timeout(std::time::Duration::from_secs(30))
                        .send()
                        .await
                        .context("could not fetch the voice preview")?
                        .error_for_status()
                        .context("the voice preview could not be fetched")?;
                    // Preview files are containers (mp3); let ffplay sniff.
                    (response, None)
                }
                None => {
                    let voice_id = voice_id
                        .filter(|v| !v.is_empty())
                        .or_else(|| {
                            let id = config.voice_id.clone();
                            (!id.is_empty()).then_some(id)
                        })
                        .context("no voice selected to preview")?;
                    let response = eleven::tts_stream(
                        &require_key()?,
                        &voice_id,
                        &config.model_id,
                        &config.output_format,
                        "Hi — this is how I'll sound in your Threadknot conversations.",
                        &config.voice_settings,
                    )
                    .await?;
                    (response, playback::pcm_sample_rate(&config.output_format))
                }
            };

            let mut sink = playback::FfplaySink::spawn(pcm_rate)?;
            let cancel = tokio_util::sync::CancellationToken::new();
            state.voice.replace_preview(Some(cancel.clone()));
            tokio::spawn(async move {
                if playback::pump(response, &mut sink, &cancel)
                    .await
                    .unwrap_or(false)
                {
                    let _ = sink.finish().await;
                }
            });
            Ok(serde_json::json!({}))
        }
        "voice.preview.stop" => {
            state.voice.replace_preview(None);
            Ok(serde_json::json!({}))
        }
        // The conversation itself.
        "voice.session.start" => {
            let thread_id = payload
                .get("threadId")
                .and_then(serde_json::Value::as_str)
                .context("missing threadId")?;
            // A preview and a session share the speakers.
            state.voice.replace_preview(None);
            let session_id = session::start(state, thread_id)?;
            Ok(serde_json::json!({ "sessionId": session_id }))
        }
        "voice.session.stop" => {
            let session_id = payload
                .get("sessionId")
                .and_then(serde_json::Value::as_str)
                .context("missing sessionId")?;
            let _ = state.voice.with_session(session_id, |ctl| ctl.cancel.cancel());
            Ok(serde_json::json!({}))
        }
        "voice.mute" => {
            let session_id = payload
                .get("sessionId")
                .and_then(serde_json::Value::as_str)
                .context("missing sessionId")?;
            let muted = payload
                .get("muted")
                .and_then(serde_json::Value::as_bool)
                .context("missing muted")?;
            state.voice.with_session(session_id, |ctl| {
                ctl.muted.store(muted, std::sync::atomic::Ordering::Relaxed)
            })?;
            Ok(serde_json::json!({}))
        }
        "voice.interrupt" => {
            let session_id = payload
                .get("sessionId")
                .and_then(serde_json::Value::as_str)
                .context("missing sessionId")?;
            state
                .voice
                .with_session(session_id, |ctl| ctl.interrupt.notify_one())?;
            Ok(serde_json::json!({}))
        }
        _ => anyhow::bail!("unknown request: {kind}"),
    }
}

/// Masked reminder of which key is saved: the last two characters, or nothing
/// for keys too short to safely reveal any of.
fn key_hint(key: &str) -> Option<String> {
    let key = key.trim();
    let chars: Vec<char> = key.chars().collect();
    (chars.len() >= 8).then(|| chars[chars.len() - 2..].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice_with(config: VoiceConfig) -> Voice {
        Voice {
            path: PathBuf::new(),
            config: Mutex::new(config),
            preview: Mutex::new(None),
            session: Mutex::new(None),
            stt: tokio::sync::Mutex::new(session::SttState::default()),
            revision: std::sync::atomic::AtomicU64::new(0),
        }
    }

    #[test]
    fn public_settings_never_expose_the_api_key() {
        let voice = voice_with(VoiceConfig {
            api_key: "sk_super-secret-key-42".into(),
            ..Default::default()
        });
        let public = voice.settings();
        assert_eq!(public["hasApiKey"], true);
        assert_eq!(public["keyHint"], "42");
        assert!(!public.to_string().contains("super-secret"));
        let capability = voice.capability(true);
        assert!(!capability.to_string().contains("super-secret"));
    }

    #[test]
    fn key_hint_reveals_nothing_of_short_keys() {
        assert_eq!(key_hint("short"), None);
        assert_eq!(key_hint(""), None);
        assert_eq!(key_hint("sk_1234567890").as_deref(), Some("90"));
    }

    #[test]
    fn save_preserves_the_key_unless_replaced_and_clears_on_empty() {
        let dir = std::env::temp_dir().join(format!("tk-voice-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let voice = Voice::open(&dir).unwrap();

        voice
            .configure(VoiceConfigInput {
                api_key: Some("sk_first-key-value".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(voice.has_api_key());

        // Absent key: preserved across an unrelated save.
        voice
            .configure(VoiceConfigInput {
                enabled: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert!(voice.has_api_key());
        assert!(!voice.config().enabled);

        // Reload from disk: the key round-trips.
        let reloaded = Voice::open(&dir).unwrap();
        assert_eq!(reloaded.api_key().as_deref(), Some("sk_first-key-value"));

        // Explicit empty string clears.
        voice
            .configure(VoiceConfigInput {
                api_key: Some("".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(!voice.has_api_key());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn configure_rejects_out_of_range_tuning() {
        let voice = voice_with(VoiceConfig::default());
        let err = voice
            .configure(VoiceConfigInput {
                voice_settings: Some(VoiceTuning {
                    stability: Some(1.4),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("stability"));
        let err = voice
            .configure(VoiceConfigInput {
                end_silence_ms: Some(50),
                ..Default::default()
            })
            .unwrap_err();
        assert!(err.to_string().contains("endSilenceMs"));
    }
}
