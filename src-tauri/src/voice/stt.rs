//! Speech-to-text for voice sessions: a warm faster-whisper sidecar, an
//! energy-based utterance endpointer, and the whisper-CLI fallback.
//!
//! Dictation shells out to the `whisper` CLI per clip, paying a Python start
//! and a full model load every time — fine for one dictated prompt, fatal for
//! turn-taking. The sidecar (`scripts/voice_stt.py`, embedded at build time)
//! loads the model once and transcribes over a JSON-lines pipe in well under a
//! second on this machine's GPU. If Python or faster-whisper is missing, each
//! utterance WAV falls back to dictation's CLI path instead.

use anyhow::{anyhow, Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

/// The sidecar source, shipped inside the binary and written to the data dir
/// at spawn — packaged builds need no resource plumbing, dev edits apply on
/// the next session.
const SIDECAR_SOURCE: &str = include_str!("../../../scripts/voice_stt.py");

/// First-ever spawn may download the model; later spawns just load it.
const READY_TIMEOUT: Duration = Duration::from_secs(300);
/// A warm model answers in well under this; the budget covers a CPU fallback
/// chewing on a long utterance.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    /// "cuda" or "cpu", from the ready event — shown in session status.
    pub device: String,
    next_id: u64,
}

fn python_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("THREADKNOT_VOICE_PYTHON") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    crate::agents::resolve_bin("python").or_else(|| crate::agents::resolve_bin("python3"))
}

impl Sidecar {
    /// Write the embedded script into the data dir and start it. Resolves once
    /// the model is loaded (the `ready` event) so the first utterance never
    /// races the load.
    pub async fn spawn() -> Result<Self> {
        let python = python_bin().ok_or_else(|| anyhow!("Python is not installed"))?;
        let script = crate::store::data_dir().join("voice_stt.py");
        let tmp = script.with_extension("py.tmp");
        std::fs::write(&tmp, SIDECAR_SOURCE).context("could not write the STT sidecar script")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &script).context("could not write the STT sidecar script")?;

        let mut cmd = Command::new(python);
        cmd.arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::agents::no_console(&mut cmd);
        let mut child = cmd.spawn().context("could not start the STT sidecar")?;
        let stdin = child.stdin.take().context("sidecar stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("sidecar stdout")?);
        let mut sidecar = Self {
            child,
            stdin,
            stdout,
            device: String::new(),
            next_id: 0,
        };

        let ready = tokio::time::timeout(READY_TIMEOUT, sidecar.read_event()).await;
        match ready {
            Ok(Ok(ev)) if ev["ev"] == "ready" => {
                sidecar.device = ev["device"].as_str().unwrap_or("cpu").to_string();
                Ok(sidecar)
            }
            Ok(Ok(ev)) => Err(anyhow!(
                "STT sidecar failed: {}",
                ev["error"].as_str().unwrap_or("unknown error")
            )),
            Ok(Err(e)) => Err(e.context("STT sidecar exited during startup")),
            Err(_) => {
                let _ = sidecar.child.kill().await;
                Err(anyhow!("STT sidecar took too long to load the model"))
            }
        }
    }

    async fn read_event(&mut self) -> Result<serde_json::Value> {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).await?;
            anyhow::ensure!(n > 0, "STT sidecar closed its pipe");
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                return Ok(v);
            }
        }
    }

    /// Transcribe one utterance WAV. One request in flight at a time — there
    /// is one conversation.
    pub async fn transcribe(&mut self, wav: &Path) -> Result<String> {
        self.next_id += 1;
        let id = format!("u{}", self.next_id);
        let request = serde_json::json!({
            "op": "transcribe",
            "id": id,
            "path": wav.to_string_lossy(),
        });
        self.stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .context("STT sidecar went away")?;
        self.stdin.flush().await.context("STT sidecar went away")?;

        let deadline = tokio::time::Instant::now() + TRANSCRIBE_TIMEOUT;
        loop {
            let ev = tokio::time::timeout_at(deadline, self.read_event())
                .await
                .map_err(|_| anyhow!("transcription timed out"))??;
            if ev["id"] != serde_json::Value::String(id.clone()) {
                continue; // stale reply from an abandoned request
            }
            return match ev["ev"].as_str() {
                Some("transcript") => Ok(crate::dictation::clean(
                    ev["text"].as_str().unwrap_or_default(),
                )),
                Some("error") => Err(anyhow!(
                    "transcription failed: {}",
                    ev["error"].as_str().unwrap_or("unknown error")
                )),
                _ => continue,
            };
        }
    }

    pub async fn kill(mut self) {
        let _ = self.child.kill().await;
    }
}

/// Transcribe with the per-clip whisper CLI — the fallback when the sidecar
/// cannot run. Same cleaning as dictation.
pub async fn transcribe_cli(wav: &Path) -> Result<String> {
    let dir = wav.parent().unwrap_or_else(|| Path::new("."));
    crate::dictation::transcribe_local(wav, dir).await
}

// ---- Utterance endpointing ---------------------------------------------------

/// Capture format: 16 kHz mono s16le, what Whisper wants.
pub const SAMPLE_RATE: u32 = 16_000;
/// Analysis frame; 30 ms is fine-grained enough for barge-in to feel instant.
pub const FRAME_MS: u32 = 30;
pub const FRAME_BYTES: usize = (SAMPLE_RATE as usize / 1000) * FRAME_MS as usize * 2;

/// Sustained speech required to open an utterance (rejects coughs and clicks).
const START_MS: u32 = 240;
/// Audio kept from before the trigger so the first word isn't clipped.
const PREROLL_MS: u32 = 510;
/// Nobody's conversational turn runs longer than this.
const MAX_UTTERANCE_MS: u32 = 90_000;
/// An utterance shorter than this can't hold a word (dictation's rule).
const MIN_UTTERANCE_MS: u32 = 400;
/// Speech must clear the noise floor by this much.
const MARGIN_DB: f32 = 12.0;
/// ...and never counts below this absolute level (a dead-quiet room's floor
/// sits far lower; -45 dBFS still catches soft speech).
const MIN_THRESHOLD_DB: f32 = -45.0;

pub struct Endpointer {
    /// Silence that closes an utterance (configurable; default 700 ms).
    end_silence_ms: u32,
    /// Extra threshold while the assistant is speaking, so playback bleed
    /// doesn't read as barge-in.
    extra_margin_db: f32,
    /// EWMA of non-speech frame levels — the room's noise floor.
    noise_floor_db: f32,
    speaking: bool,
    run_ms: u32,
    silence_ms: u32,
    preroll: std::collections::VecDeque<Vec<u8>>,
    utterance: Vec<u8>,
    /// 0..1 level of the last frame, for the UI's listening ring.
    pub level: f32,
}

pub enum Endpoint {
    /// Still listening; nothing to do.
    Quiet,
    /// Speech opened — the UI flips to "listening…" emphasis / barge-in fires.
    Started,
    /// An utterance closed; here is its 16 kHz mono PCM.
    Utterance(Vec<u8>),
}

impl Endpointer {
    pub fn new(end_silence_ms: u32) -> Self {
        Self {
            end_silence_ms: end_silence_ms.clamp(200, 5000),
            extra_margin_db: 0.0,
            noise_floor_db: -60.0,
            speaking: false,
            run_ms: 0,
            silence_ms: 0,
            preroll: std::collections::VecDeque::new(),
            utterance: Vec::new(),
            level: 0.0,
        }
    }

    /// While the assistant speaks, require more level to call it speech —
    /// speaker bleed raises the room. (Headphones make barge-in crisp; the
    /// settings hint says so.)
    pub fn set_playback_guard(&mut self, on: bool) {
        self.extra_margin_db = if on { 10.0 } else { 0.0 };
    }

    /// Feed one FRAME_BYTES frame of s16le PCM.
    pub fn push(&mut self, frame: &[u8]) -> Endpoint {
        let db = dbfs(frame);
        self.level = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let threshold =
            (self.noise_floor_db + MARGIN_DB + self.extra_margin_db).max(MIN_THRESHOLD_DB);
        let loud = db > threshold;

        if !self.speaking {
            if !loud {
                // Only quiet frames teach the floor, so speech can't raise it.
                self.noise_floor_db = self.noise_floor_db * 0.95 + db * 0.05;
            }
            self.preroll.push_back(frame.to_vec());
            while self.preroll.len() * FRAME_MS as usize > PREROLL_MS as usize {
                self.preroll.pop_front();
            }
            if loud {
                self.run_ms += FRAME_MS;
                if self.run_ms >= START_MS {
                    self.speaking = true;
                    self.silence_ms = 0;
                    self.utterance = self.preroll.iter().flatten().copied().collect();
                    return Endpoint::Started;
                }
            } else {
                self.run_ms = 0;
            }
            return Endpoint::Quiet;
        }

        self.utterance.extend_from_slice(frame);
        if loud {
            self.silence_ms = 0;
        } else {
            self.silence_ms += FRAME_MS;
        }
        let ms = (self.utterance.len() / FRAME_BYTES) as u32 * FRAME_MS;
        if self.silence_ms >= self.end_silence_ms || ms >= MAX_UTTERANCE_MS {
            self.speaking = false;
            self.run_ms = 0;
            self.preroll.clear();
            let pcm = std::mem::take(&mut self.utterance);
            if ms.saturating_sub(self.silence_ms) < MIN_UTTERANCE_MS {
                return Endpoint::Quiet; // a blip, not a word
            }
            return Endpoint::Utterance(pcm);
        }
        Endpoint::Quiet
    }

    /// Drop any half-open utterance (mute, interrupt without capture).
    pub fn reset(&mut self) {
        self.speaking = false;
        self.run_ms = 0;
        self.silence_ms = 0;
        self.utterance.clear();
        self.preroll.clear();
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }
}

/// Mean level of an s16le frame in dBFS.
fn dbfs(frame: &[u8]) -> f32 {
    if frame.len() < 2 {
        return -90.0;
    }
    let mut sum = 0f64;
    let mut n = 0f64;
    for pair in frame.chunks_exact(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]) as f64 / 32768.0;
        sum += s * s;
        n += 1.0;
    }
    let rms = (sum / n).sqrt();
    if rms <= 0.0 {
        -90.0
    } else {
        (20.0 * rms.log10()) as f32
    }
}

/// Wrap raw 16 kHz mono s16le PCM in a WAV header and write it to `path`.
pub fn write_wav(path: &Path, pcm: &[u8]) -> Result<()> {
    let mut f = std::fs::File::create(path).context("could not write the utterance file")?;
    let data_len = pcm.len() as u32;
    let byte_rate = SAMPLE_RATE * 2;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    f.write_all(pcm)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(amplitude: i16) -> Vec<u8> {
        // A square-ish wave at the given amplitude, one frame long.
        let samples = FRAME_BYTES / 2;
        let mut out = Vec::with_capacity(FRAME_BYTES);
        for i in 0..samples {
            let s = if i % 2 == 0 { amplitude } else { -amplitude };
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn quiet() -> Vec<u8> {
        frame(30) // ~-60 dBFS
    }
    fn loud() -> Vec<u8> {
        frame(6000) // ~-15 dBFS
    }

    #[test]
    fn a_click_does_not_open_an_utterance() {
        let mut e = Endpointer::new(700);
        for _ in 0..20 {
            assert!(matches!(e.push(&quiet()), Endpoint::Quiet));
        }
        // Two loud frames (60 ms) — below the 240 ms start requirement.
        assert!(matches!(e.push(&loud()), Endpoint::Quiet));
        assert!(matches!(e.push(&loud()), Endpoint::Quiet));
        for _ in 0..10 {
            assert!(matches!(e.push(&quiet()), Endpoint::Quiet));
        }
        assert!(!e.is_speaking());
    }

    #[test]
    fn speech_then_silence_yields_the_utterance_with_preroll() {
        let mut e = Endpointer::new(700);
        for _ in 0..30 {
            e.push(&quiet());
        }
        let mut started = false;
        // ~1.2 s of speech...
        let mut result = None;
        for _ in 0..40 {
            match e.push(&loud()) {
                Endpoint::Started => started = true,
                Endpoint::Utterance(_) => panic!("closed during speech"),
                Endpoint::Quiet => {}
            }
        }
        assert!(started, "sustained speech must open an utterance");
        // ...then 700 ms of silence closes it.
        for _ in 0..30 {
            if let Endpoint::Utterance(pcm) = e.push(&quiet()) {
                result = Some(pcm);
                break;
            }
        }
        let pcm = result.expect("silence must close the utterance");
        // Speech (40 frames) + preroll + tail silence, all in whole frames.
        assert!(pcm.len() >= 40 * FRAME_BYTES);
        assert_eq!(pcm.len() % FRAME_BYTES, 0);
        assert!(!e.is_speaking());
    }

    #[test]
    fn playback_guard_raises_the_bar() {
        let mut e = Endpointer::new(700);
        for _ in 0..30 {
            e.push(&quiet());
        }
        e.set_playback_guard(true);
        // Floor sits near -60: threshold without the guard is max(-60+12, -45)
        // = -45 dBFS, with it max(-60+22, -45) = -38. Pick a level between:
        let mid = frame(292); // ~-41 dBFS: clears -45, not -38
        for _ in 0..20 {
            assert!(matches!(e.push(&mid), Endpoint::Quiet));
        }
        assert!(!e.is_speaking());
        e.set_playback_guard(false);
        // The sub-threshold bleed trained the floor upward (by design); let a
        // quiet room settle it back down before checking the unguarded bar.
        for _ in 0..120 {
            e.push(&quiet());
        }
        for _ in 0..10 {
            e.push(&mid);
        }
        assert!(e.is_speaking(), "same level opens speech without the guard");
    }

    #[test]
    fn max_length_force_closes() {
        let mut e = Endpointer::new(5000);
        for _ in 0..30 {
            e.push(&quiet());
        }
        let mut closed = false;
        for _ in 0..4000 {
            if let Endpoint::Utterance(_) = e.push(&loud()) {
                closed = true;
                break;
            }
        }
        assert!(closed, "an endless monologue still closes at the cap");
    }

    #[test]
    fn wav_header_is_well_formed() {
        let dir = std::env::temp_dir().join(format!("tk-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        write_wav(&path, &vec![0u8; 3200]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(bytes.len(), 44 + 3200);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
