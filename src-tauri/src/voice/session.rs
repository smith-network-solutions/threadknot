//! The live voice conversation: one task owning mic → STT → agent → TTS →
//! speakers, publishing every state change as a `voice.state` broadcast.
//!
//! There is one microphone and one pair of speakers, so there is one session
//! slot (dictation's doctrine). Everything hangs off one CancellationToken;
//! `voice.session.stop` cancels it and every await in the loop is selected
//! against it. Barge-in is the tightest path available: kill the ffplay child
//! (audio dies in the OS buffer), drop queued chunks, `hub.interrupt` the
//! thread — and the utterance that interrupted keeps being captured.

use super::playback::FfplaySink;
use super::stt::{Endpoint, Endpointer, FRAME_BYTES};
use super::{playback, stt, Verbosity, Voice, VoiceConfig};
use crate::agents::Hub;
use crate::protocol::{AgentEvent, ServerMessage, VoiceError};
use anyhow::{anyhow, Context, Result};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc, Notify};
use tokio_util::sync::CancellationToken;

/// What the agent is told about speaking aloud, by verbosity. Not persisted —
/// prepended to the driver text only (see `Hub::start_turn_voice`).
fn preface(verbosity: Verbosity) -> &'static str {
    match verbosity {
        Verbosity::Concise => {
            "[voice mode] The user is speaking aloud and will hear your reply as speech. \
             Answer in one to three short conversational sentences with no markdown, code \
             blocks, tables, or lists. If code or long detail is needed, do the work, put \
             it in your normal output, and say a one-line summary aloud. Never drop safety-\
             critical information."
        }
        Verbosity::Normal => {
            "[voice mode] The user is speaking aloud and will hear your reply as speech. \
             Keep it conversational and reasonably brief; avoid markdown, code blocks, \
             tables, and long enumerations in the spoken reply — put those in your normal \
             output and summarize them aloud."
        }
        Verbosity::Detailed => {
            "[voice mode] The user is speaking aloud and will hear your reply as speech. \
             Answer as fully as useful, but in plain speakable prose — no markdown, code \
             blocks, or tables aloud; put those in your normal output."
        }
    }
}

/// Handle the verbs own: everything needed to reach into the running session.
pub struct SessionCtl {
    pub id: String,
    pub thread_id: String,
    pub cancel: CancellationToken,
    pub muted: Arc<AtomicBool>,
    pub interrupt: Arc<Notify>,
}

/// The warm STT sidecar, shared across sessions so the model survives between
/// conversations. `fallback` remembers that the sidecar cannot run here.
#[derive(Default)]
pub struct SttState {
    sidecar: Option<stt::Sidecar>,
    fallback: bool,
    last_used: Option<Instant>,
}

impl SttState {
    pub(crate) fn idle_since(&self) -> Option<Instant> {
        self.sidecar.is_some().then_some(self.last_used).flatten()
    }

    pub(crate) fn take_sidecar(&mut self) -> Option<stt::Sidecar> {
        self.sidecar.take()
    }
}

const SIDECAR_IDLE_REAP: Duration = Duration::from_secs(300);

pub fn start(state: &crate::server::ServerState, thread_id: &str) -> Result<String> {
    let voice = &state.voice;
    let hub = &state.hub;
    let config = voice.config();
    anyhow::ensure!(config.enabled, "voice output is turned off in Settings → Voice");
    anyhow::ensure!(
        !config.api_key.trim().is_empty(),
        "No ElevenLabs API key saved — add one in Settings → Voice"
    );
    anyhow::ensure!(
        !config.voice_id.trim().is_empty(),
        "Choose a voice in Settings → Voice first"
    );
    let thread = hub
        .store
        .thread(thread_id)
        .ok_or_else(|| anyhow!("unknown thread"))?;
    anyhow::ensure!(
        !state.dictation.is_recording(),
        "dictation is using the microphone — stop it first"
    );

    // One session: starting a new one replaces whatever was running.
    voice.end_session();

    let id = crate::protocol::new_id();
    let ctl = SessionCtl {
        id: id.clone(),
        thread_id: thread.id.clone(),
        cancel: CancellationToken::new(),
        muted: Arc::new(AtomicBool::new(false)),
        interrupt: Arc::new(Notify::new()),
    };
    let cancel = ctl.cancel.clone();
    let muted = Arc::clone(&ctl.muted);
    let interrupt = Arc::clone(&ctl.interrupt);
    voice.set_session(ctl);

    let voice = Arc::clone(voice);
    let hub = Arc::clone(hub);
    let session_id = id.clone();
    let thread_id = thread.id.clone();
    tokio::spawn(async move {
        let run = run_session(
            &voice,
            &hub,
            &session_id,
            &thread_id,
            config,
            cancel.clone(),
            muted,
            interrupt,
        )
        .await;
        if let Err(e) = run {
            // Fatal setup/loop error: tell the UI, then fall to idle.
            voice.publish_frame(
                &hub,
                Some(&session_id),
                Some(&thread_id),
                "error",
                false,
                None,
                None,
                Some(classify_error(&e)),
                None,
            );
        }
        if voice.clear_session(&session_id) {
            voice.publish_idle(&hub);
        }
        // The sidecar stays warm for the next conversation, then reaps.
        let voice2 = Arc::clone(&voice);
        tokio::spawn(async move {
            tokio::time::sleep(SIDECAR_IDLE_REAP).await;
            voice2.reap_idle_sidecar(SIDECAR_IDLE_REAP).await;
        });
    });
    Ok(id)
}

/// Map an error to a stable code + whether the session can keep going.
fn classify_error(e: &anyhow::Error) -> VoiceError {
    let text = format!("{e:#}");
    let (code, recoverable) = if text.contains("rejected the API key") {
        ("tts_auth", false)
    } else if text.contains("credits are exhausted") {
        ("tts_quota", false)
    } else if text.contains("rate-limiting") {
        ("tts_rate_limited", true)
    } else if text.contains("microphone") || text.contains("record") {
        ("mic_unavailable", false)
    } else if text.contains("ffplay") {
        ("playback_unavailable", false)
    } else if text.contains("transcription") || text.contains("sidecar") {
        ("stt_failed", true)
    } else if text.contains("busy") || text.contains("mid-turn") {
        ("agent_busy", true)
    } else {
        ("voice_error", true)
    };
    VoiceError {
        code: code.into(),
        message: text,
        recoverable,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    voice: &Arc<Voice>,
    hub: &Arc<Hub>,
    session_id: &str,
    thread_id: &str,
    config: VoiceConfig,
    cancel: CancellationToken,
    muted: Arc<AtomicBool>,
    interrupt: Arc<Notify>,
) -> Result<()> {
    let scratch = std::env::temp_dir().join(format!("threadknot-voice-{session_id}"));
    std::fs::create_dir_all(&scratch).context("could not create the voice scratch dir")?;

    let publish = |state: &str, last: Option<&str>, detail: Option<&str>, err: Option<VoiceError>, level: Option<f32>| {
        voice.publish_frame(
            hub,
            Some(session_id),
            Some(thread_id),
            state,
            muted.load(Ordering::Relaxed),
            last,
            detail,
            err,
            level,
        );
    };

    // Mic first: if it can't open, the session is dead on arrival.
    let (mut ffmpeg, mut mic_rx) = spawn_mic(&cancel).await?;
    let mut endpointer = Endpointer::new(config.end_silence_ms as u32);
    publish("listening", None, None, None, None);

    let mut last_utterance = String::new();
    let mut utterance_n = 0u64;
    let mut level_published = Instant::now();

    enum MicWait {
        Cancelled,
        Frame(Vec<u8>),
        Dead,
    }

    let result: Result<()> = loop {
        // The select is an expression so its borrows end before the handling
        // below, which needs mic_rx and endpointer itself (for run_turn).
        let wait = tokio::select! {
            _ = cancel.cancelled() => MicWait::Cancelled,
            frame = mic_rx.recv() => frame.map(MicWait::Frame).unwrap_or(MicWait::Dead),
        };
        match wait {
            MicWait::Cancelled => break Ok(()),
            MicWait::Dead => break Err(anyhow!("the microphone stream ended unexpectedly")),
            MicWait::Frame(frame) => {
                if muted.load(Ordering::Relaxed) {
                    endpointer.reset();
                    continue;
                }
                match endpointer.push(&frame) {
                    Endpoint::Quiet => {
                        // A few times a second is plenty for the level ring.
                        if level_published.elapsed() >= Duration::from_millis(150) {
                            level_published = Instant::now();
                            publish("listening", none_if_empty(&last_utterance), None, None, Some(endpointer.level));
                        }
                    }
                    Endpoint::Started => {
                        publish("listening", none_if_empty(&last_utterance), None, None, Some(endpointer.level));
                    }
                    Endpoint::Utterance(pcm) => {
                        publish("transcribing", None, None, None, None);
                        utterance_n += 1;
                        let wav = scratch.join(format!("utt-{utterance_n}.wav"));
                        let text = match transcribe(voice, &wav, &pcm, &publish).await {
                            Ok(text) => text,
                            Err(e) => {
                                publish("error", None, None, Some(classify_error(&e)), None);
                                publish("listening", None, None, None, None);
                                continue;
                            }
                        };
                        let _ = std::fs::remove_file(&wav);
                        if text.is_empty() {
                            publish("listening", None, None, None, None);
                            continue;
                        }
                        last_utterance = text.clone();

                        // The whole agent turn runs inside this arm; mic frames
                        // for barge-in are read from mic_rx by the turn loop.
                        let outcome = run_turn(
                            voice, hub, session_id, thread_id, &config, &cancel,
                            &muted, &interrupt, &mut mic_rx, &mut endpointer,
                            &text, &publish,
                        )
                        .await;
                        match outcome {
                            Ok(()) => {}
                            Err(e) => {
                                let err = classify_error(&e);
                                let fatal = !err.recoverable;
                                publish("error", Some(&last_utterance), None, Some(err), None);
                                if fatal {
                                    break Err(e);
                                }
                            }
                        }
                        if cancel.is_cancelled() {
                            break Ok(());
                        }
                        if !config.auto_listen {
                            // Manual mode: the mic goes cold until unmuted.
                            muted.store(true, Ordering::Relaxed);
                        }
                        endpointer.reset();
                        publish("listening", Some(&last_utterance), None, None, None);
                    }
                }
            }
        }
    };

    let _ = ffmpeg.kill().await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn none_if_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// The ffmpeg mic capture: 16 kHz mono s16le on stdout, framed into
/// FRAME_BYTES chunks on a channel.
async fn spawn_mic(
    cancel: &CancellationToken,
) -> Result<(tokio::process::Child, mpsc::Receiver<Vec<u8>>)> {
    let (format, device) = crate::dictation::capture_args().await?;
    let mut cmd = Command::new(crate::dictation::tool("ffmpeg")?);
    cmd.args(["-hide_banner", "-loglevel", "error", "-f", format, "-i", &device])
        .args(["-ac", "1", "-ar", &stt::SAMPLE_RATE.to_string(), "-f", "s16le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::agents::no_console(&mut cmd);
    let mut child = cmd
        .spawn()
        .context("could not start ffmpeg to record the microphone")?;
    let mut stdout = child.stdout.take().context("mic stdout")?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>(32);
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; FRAME_BYTES];
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                read = stdout.read_exact(&mut buf) => {
                    if read.is_err() {
                        break; // ffmpeg died; the session loop sees the closed channel
                    }
                    if tx.send(buf.clone()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok((child, rx))
}

/// STT with the warm sidecar, falling back to the whisper CLI.
async fn transcribe(
    voice: &Arc<Voice>,
    wav: &std::path::Path,
    pcm: &[u8],
    publish: &impl Fn(&str, Option<&str>, Option<&str>, Option<VoiceError>, Option<f32>),
) -> Result<String> {
    stt::write_wav(wav, pcm)?;
    let mut state = voice.stt.lock().await;
    state.last_used = Some(Instant::now());
    if !state.fallback && state.sidecar.is_none() {
        publish("transcribing", None, Some("loading the speech model…"), None, None);
        match stt::Sidecar::spawn().await {
            Ok(s) => state.sidecar = Some(s),
            Err(e) => {
                tracing::warn!("voice STT sidecar unavailable, using whisper CLI: {e:#}");
                state.fallback = true;
                publish(
                    "transcribing",
                    None,
                    Some("faster-whisper unavailable — using the slower whisper CLI"),
                    None,
                    None,
                );
            }
        }
    }
    if let Some(sidecar) = state.sidecar.as_mut() {
        match sidecar.transcribe(wav).await {
            Ok(text) => return Ok(text),
            Err(e) => {
                // A dead pipe gets one respawn; a transcription error passes up.
                if e.to_string().contains("sidecar") {
                    state.sidecar = None;
                    if let Ok(mut s) = stt::Sidecar::spawn().await {
                        let text = s.transcribe(wav).await;
                        state.sidecar = Some(s);
                        return text;
                    }
                    state.fallback = true;
                } else {
                    return Err(e);
                }
            }
        }
    }
    stt::transcribe_cli(wav).await
}

/// Everything between "the user said something" and "the reply finished
/// playing": start the agent turn, chunk its stream, speak it, and watch the
/// mic + interrupt signal for barge-in the whole way through.
#[allow(clippy::too_many_arguments)]
async fn run_turn(
    voice: &Arc<Voice>,
    hub: &Arc<Hub>,
    session_id: &str,
    thread_id: &str,
    config: &VoiceConfig,
    cancel: &CancellationToken,
    muted: &Arc<AtomicBool>,
    interrupt: &Arc<Notify>,
    mic_rx: &mut mpsc::Receiver<Vec<u8>>,
    endpointer: &mut Endpointer,
    text: &str,
    publish: &impl Fn(&str, Option<&str>, Option<&str>, Option<VoiceError>, Option<f32>),
) -> Result<()> {
    // Refuse to talk over a busy thread; interrupting typed work uninvited
    // would be worse than asking.
    if let Some(thread) = hub.store.thread(thread_id) {
        anyhow::ensure!(
            thread.status == crate::protocol::ThreadStatus::Idle,
            "that thread is mid-turn — stop it or wait before speaking"
        );
    }

    // Subscribe BEFORE the turn starts so no delta is missed.
    let events = hub.broadcast.subscribe();
    hub.start_turn_voice(
        thread_id,
        text.to_string(),
        Some(preface(config.verbosity).to_string()),
    )?;
    publish("thinking", Some(text), None, None, None);

    // The speaker runs as its own task: it must never be cancelled mid-chunk
    // by a select racing it against mic frames, so barge-in reaches it through
    // a token instead. The token also threads into the audio pump, which makes
    // "stop the sound" take effect inside a chunk, not between chunks.
    let turn_token = CancellationToken::new();
    let mut speaker = Speaker {
        voice: Arc::clone(voice),
        hub: Arc::clone(hub),
        session_id: session_id.to_string(),
        thread_id: thread_id.to_string(),
        config: config.clone(),
        events,
        token: turn_token.clone(),
        chunker: super::chunker::Chunker::default(),
        sink: None,
        spoke: false,
        last_event: Instant::now(),
    };
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<TurnStep>();
    let speaker_task = tokio::spawn(async move {
        let outcome = speaker.run(&update_tx).await;
        match outcome {
            Ok(done) => {
                let _ = update_tx.send(done);
            }
            Err(e) => {
                let _ = update_tx.send(TurnStep::Failed(format!("{e:#}")));
            }
        }
        speaker.cleanup().await;
    });

    endpointer.set_playback_guard(true);
    let barge_in = config.barge_in;

    enum TurnWait {
        Cancelled,
        Interrupted,
        Frame(Option<Vec<u8>>),
        Update(Option<TurnStep>),
    }

    let result = loop {
        let wait = tokio::select! {
            _ = cancel.cancelled() => TurnWait::Cancelled,
            _ = interrupt.notified() => TurnWait::Interrupted,
            frame = mic_rx.recv(), if barge_in => TurnWait::Frame(frame),
            update = update_rx.recv() => TurnWait::Update(update),
        };
        match wait {
            TurnWait::Cancelled => {
                turn_token.cancel();
                break Ok(());
            }
            TurnWait::Interrupted => {
                turn_token.cancel();
                let _ = hub.interrupt(thread_id);
                publish("interrupted", None, None, None, None);
                break Ok(());
            }
            TurnWait::Frame(None) => {
                turn_token.cancel();
                break Err(anyhow!("the microphone stream ended unexpectedly"));
            }
            TurnWait::Frame(Some(frame)) => {
                if muted.load(Ordering::Relaxed) {
                    continue;
                }
                if let Endpoint::Started = endpointer.push(&frame) {
                    // The user is talking over the reply: cut everything and
                    // keep capturing — the open utterance survives this return
                    // and closes back in the main loop.
                    turn_token.cancel();
                    let _ = hub.interrupt(thread_id);
                    publish("interrupted", None, None, None, None);
                    break Ok(());
                }
            }
            TurnWait::Update(None) => {
                // The speaker task never ends without a final word unless it
                // panicked; the join below surfaces that.
                tracing::warn!("voice speaker task ended without reporting");
                break Ok(());
            }
            TurnWait::Update(Some(step)) => match step {
                TurnStep::Working(state) => {
                    publish(state, None, None, None, None);
                }
                TurnStep::Done => break Ok(()),
                TurnStep::Aborted => {
                    publish("interrupted", None, None, None, None);
                    break Ok(());
                }
                TurnStep::Failed(message) => break Err(anyhow!(message)),
            },
        }
    };
    // Wait for the speaker to actually release the speakers before the next
    // reply could want them (bounded — cleanup only kills processes).
    match tokio::time::timeout(Duration::from_secs(5), speaker_task).await {
        Ok(Err(join)) if join.is_panic() => {
            tracing::error!("voice speaker task panicked: {join}");
        }
        _ => {}
    }
    endpointer.set_playback_guard(false);
    result
}

enum TurnStep {
    /// Still going; the given UI state is current.
    Working(&'static str),
    Done,
    Aborted,
    Failed(String),
}

/// Owns the reply side of one turn: consumes the thread's event stream,
/// chunks assistant text, synthesizes, and plays. Lives in its own task; the
/// turn loop reaches it only through `token` and the update channel.
struct Speaker {
    voice: Arc<Voice>,
    hub: Arc<Hub>,
    session_id: String,
    thread_id: String,
    config: VoiceConfig,
    events: broadcast::Receiver<ServerMessage>,
    token: CancellationToken,
    chunker: super::chunker::Chunker,
    sink: Option<FfplaySink>,
    spoke: bool,
    last_event: Instant,
}

/// What one wait on the event stream produced.
enum SpeakerWait {
    Cancelled,
    Event(AgentEvent),
    Irrelevant,
    Lagged,
    Closed,
    Stall,
}

impl Speaker {
    async fn run(&mut self, updates: &mpsc::UnboundedSender<TurnStep>) -> Result<TurnStep> {
        loop {
            let wait = {
                let stall = tokio::time::sleep(Duration::from_millis(250));
                tokio::select! {
                    _ = self.token.cancelled() => SpeakerWait::Cancelled,
                    ev = self.events.recv() => match ev {
                        Ok(ServerMessage::Event { thread_id, event, .. })
                            if thread_id == self.thread_id => SpeakerWait::Event(event),
                        Ok(_) => SpeakerWait::Irrelevant,
                        Err(broadcast::error::RecvError::Lagged(_)) => SpeakerWait::Lagged,
                        Err(broadcast::error::RecvError::Closed) => SpeakerWait::Closed,
                    },
                    _ = stall => SpeakerWait::Stall,
                }
            };
            match wait {
                SpeakerWait::Cancelled => return Ok(TurnStep::Aborted),
                SpeakerWait::Irrelevant => {}
                SpeakerWait::Lagged => {
                    // Missed deltas (a very busy hub). The next full
                    // assistant_message re-speaks its block from the top —
                    // possibly repeating a phrase, never losing one.
                    self.chunker.abort();
                }
                SpeakerWait::Closed => {
                    anyhow::bail!("the event stream closed mid-turn");
                }
                SpeakerWait::Stall => {
                    let waited = self.last_event.elapsed().as_millis() as u64;
                    let chunks = self.chunker.stall_flush(waited);
                    self.speak(chunks, updates).await?;
                }
                SpeakerWait::Event(event) => {
                    self.last_event = Instant::now();
                    match event {
                        AgentEvent::AssistantDelta { text } => {
                            let chunks = self.chunker.push_delta(&text);
                            self.speak(chunks, updates).await?;
                        }
                        AgentEvent::AssistantMessage { text } => {
                            let chunks = self.chunker.push_message(&text);
                            self.speak(chunks, updates).await?;
                        }
                        AgentEvent::TurnCompleted { .. } => {
                            let chunks = self.chunker.finish();
                            self.speak(chunks, updates).await?;
                            if let Some(sink) = self.sink.take() {
                                // Drain the buffered tail, but stay killable:
                                // dropping the finish future drops the sink,
                                // whose child is kill_on_drop — barge-in still
                                // silences a reply that is only draining.
                                tokio::select! {
                                    _ = self.token.cancelled() => return Ok(TurnStep::Aborted),
                                    _ = sink.finish() => {}
                                }
                            }
                            self.hub.usage.kick(false);
                            return Ok(TurnStep::Done);
                        }
                        AgentEvent::TurnAborted => return Ok(TurnStep::Aborted),
                        AgentEvent::Error { message } => {
                            anyhow::bail!("the agent failed: {message}");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Sequentially synthesize and play chunks. PCM chunks concatenate
    /// seamlessly into the one ffplay stdin, so a single reply is one child.
    async fn speak(
        &mut self,
        chunks: Vec<String>,
        updates: &mpsc::UnboundedSender<TurnStep>,
    ) -> Result<()> {
        for chunk in chunks {
            if self.token.is_cancelled() {
                return Ok(());
            }
            let key = self
                .voice
                .api_key()
                .ok_or_else(|| anyhow!("the ElevenLabs API key was removed mid-session"))?;
            if !self.spoke {
                let _ = updates.send(TurnStep::Working("synthesizing"));
            }
            let started = Instant::now();
            let response = eleven_request(&key, &self.config, &chunk).await?;
            let ttfb = started.elapsed().as_millis() as u64;
            if self.sink.is_none() {
                self.sink = Some(FfplaySink::spawn(playback::pcm_sample_rate(
                    &self.config.output_format,
                ))?);
            }
            if !self.spoke {
                self.spoke = true;
                let _ = updates.send(TurnStep::Working("speaking"));
            }
            let sink = self.sink.as_mut().expect("sink just ensured");
            let token = self.token.clone();
            playback::pump(response, sink, &token).await?;
            self.voice.record_tts_metric(
                &self.session_id,
                &self.thread_id,
                &self.config.voice_id,
                &self.config.model_id,
                chunk.chars().count() as u64,
                ttfb,
                started.elapsed().as_millis() as u64,
            );
        }
        Ok(())
    }

    async fn cleanup(&mut self) {
        self.chunker.abort();
        if let Some(mut sink) = self.sink.take() {
            sink.kill().await;
        }
    }
}

/// One TTS request, with a single retry on rate-limiting.
async fn eleven_request(
    key: &str,
    config: &VoiceConfig,
    text: &str,
) -> Result<reqwest::Response> {
    let request = || {
        super::eleven::tts_stream(
            key,
            &config.voice_id,
            &config.model_id,
            &config.output_format,
            text,
            &config.voice_settings,
        )
    };
    match request().await {
        Ok(r) => Ok(r),
        Err(e) if e.to_string().contains("rate-limiting") => {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            request().await
        }
        Err(e) => Err(e),
    }
}
