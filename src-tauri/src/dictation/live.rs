//! Live dictation: the words reach the composer while they are being spoken.
//!
//! The mic streams 30 ms frames into the same energy endpointer voice sessions
//! use. Two kinds of transcript come out of it:
//!
//! - a **partial** is a guess at the sentence still being spoken, taken a
//!   couple of times a second from the audio so far. It is allowed to change.
//! - a **final** is the sentence once a pause closed it. It never changes.
//!
//! Both are pushed as `dictation.live` broadcasts carrying a segment number, so
//! the composer can keep rewriting one in-progress stretch of text and then
//! settle it, while the user edits anything else in the draft.
//!
//! Capture and transcription run as two tasks on purpose. Transcribing inline
//! would stop the loop reading the mic for as long as the model takes, ffmpeg
//! would block on a full pipe, and DirectShow would drop audio on the floor:
//! the words spoken while the previous sentence was being transcribed would
//! simply be missing. So the capture loop only ever queues work, and never
//! waits on it.

use super::{DictationConfig, Provider};
use crate::agents::Hub;
use crate::protocol::{DictationLiveFrame, ServerMessage};
use crate::voice::stt::{self, Endpoint, Endpointer, TranscribeOpts, FRAME_BYTES, FRAME_MS};
use crate::voice::{session, Voice};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// A pause this long ends a sentence. Shorter than a voice session's 700 ms:
/// nobody is waiting to reply, and a quicker close means text settles sooner.
const END_SILENCE_MS: u32 = 550;

/// Close a sentence that never pauses. Keeps every clip inside Whisper's 30 s
/// window and keeps re-transcribing the open sentence cheap.
const MAX_UTTERANCE_MS: u32 = 25_000;

/// How often the open sentence is re-transcribed for a partial.
const PARTIAL_EVERY: Duration = Duration::from_millis(600);

/// The first partial comes this soon after speech opens, so the first words
/// show up quickly instead of a full interval later.
const FIRST_PARTIAL_AFTER: Duration = Duration::from_millis(250);

/// Hard cap on one dictation. The composer stops at the same mark.
const MAX_SESSION: Duration = Duration::from_secs(600);

/// How much already-settled text is handed to the model as context.
const PROMPT_CHARS: usize = 200;

/// A mic that never rises above this is not a quiet room, it is a muted or
/// disconnected input: real hardware has a noise floor far above it.
const DEAD_MIC_DBFS: f32 = -75.0;

/// Everything a finished dictation heard.
#[derive(Debug, Default, Serialize)]
pub struct Outcome {
    /// All of it, joined. What a client that ignores the live frames inserts.
    pub text: String,
    /// The same words by segment, index = segment number, empty where a
    /// segment held no speech. A live client uses this to apply any final
    /// frame it has not seen yet, since a broadcast and this response travel
    /// separately and may arrive in either order.
    pub segments: Vec<String>,
}

struct Job {
    segment: u32,
    pcm: Vec<u8>,
    is_final: bool,
}

fn emit(hub: &Hub, recording_id: &str, kind: &str, segment: u32, text: &str) {
    let _ = hub.broadcast.send(ServerMessage::DictationLive {
        frame: DictationLiveFrame {
            recording_id: recording_id.to_string(),
            kind: kind.to_string(),
            segment,
            text: text.to_string(),
        },
    });
}

/// The tail of what has already been settled, cut on a word boundary.
fn prompt_tail(segments: &[String]) -> Option<String> {
    let joined = segments
        .iter()
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        return None;
    }
    let chars: Vec<char> = joined.chars().collect();
    if chars.len() <= PROMPT_CHARS {
        return Some(joined);
    }
    let tail: String = chars[chars.len() - PROMPT_CHARS..].iter().collect();
    Some(match tail.split_once(' ') {
        Some((_, rest)) if !rest.is_empty() => rest.to_string(),
        _ => tail,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    id: String,
    voice: Arc<Voice>,
    hub: Arc<Hub>,
    config: DictationConfig,
    mut ffmpeg: Child,
    mut mic: mpsc::Receiver<Vec<u8>>,
    stop: CancellationToken,
    cancel: CancellationToken,
) -> Result<Outcome> {
    let scratch = std::env::temp_dir().join(format!("threadknot-dictation-{id}"));
    std::fs::create_dir_all(&scratch)
        .map_err(|e| anyhow!("could not create the dictation scratch dir: {e}"))?;

    // Partials only make sense against the warm model: the CLI takes seconds
    // per clip and a remote API bills per request. Load the model now, while
    // the user is drawing breath, and learn which of the two we got.
    let partials = Arc::new(AtomicBool::new(false));
    if config.provider == Provider::Local {
        let (voice, hub, id, partials) = (
            Arc::clone(&voice),
            Arc::clone(&hub),
            id.clone(),
            Arc::clone(&partials),
        );
        tokio::spawn(async move {
            let warm = session::warm_stt(&voice, &|detail: &str| {
                emit(&hub, &id, "note", 0, detail);
            })
            .await;
            partials.store(warm, Ordering::Relaxed);
            emit(&hub, &id, "note", 0, "");
        });
    }

    // ---- the transcription worker ------------------------------------------
    let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
    let inflight = Arc::new(AtomicUsize::new(0));
    let worker = {
        let (voice, hub, id, cancel, inflight, scratch) = (
            Arc::clone(&voice),
            Arc::clone(&hub),
            id.clone(),
            cancel.clone(),
            Arc::clone(&inflight),
            scratch.clone(),
        );
        tokio::spawn(async move {
            let mut segments: Vec<String> = Vec::new();
            let mut last_error: Option<anyhow::Error> = None;
            let mut clip = 0u64;
            while let Some(job) = queue.recv().await {
                if cancel.is_cancelled() {
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                clip += 1;
                let wav = scratch.join(format!("clip-{clip}.wav"));
                let result = async {
                    stt::write_wav(&wav, &job.pcm)?;
                    match config.provider {
                        Provider::Local => {
                            let opts = TranscribeOpts {
                                beam: Some(if job.is_final { 5 } else { 1 }),
                                prompt: prompt_tail(&segments),
                            };
                            session::transcribe_shared(&voice, &wav, &opts, &|detail: &str| {
                                emit(&hub, &id, "note", job.segment, detail);
                            })
                            .await
                        }
                        Provider::Api => super::transcribe_api(&wav, &config).await,
                    }
                }
                .await;
                let _ = std::fs::remove_file(&wav);
                inflight.fetch_sub(1, Ordering::SeqCst);

                match result {
                    Ok(text) => {
                        if !cancel.is_cancelled() {
                            if job.is_final {
                                emit(&hub, &id, "final", job.segment, &text);
                            } else {
                                // The model closes whatever it is given with a
                                // full stop, so a guess cut mid-sentence reads
                                // "take a look." and the dot blinks in and out
                                // as the sentence grows. The sentence is not
                                // over until its final says so.
                                emit(&hub, &id, "partial", job.segment, text.trim_end_matches('.'));
                            }
                        }
                        if job.is_final {
                            segments.push(text);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("dictation segment {} failed: {e:#}", job.segment);
                        // A partial that failed is just a guess that never
                        // arrived. A final has to be answered, or the composer
                        // would keep the last guess on screen as if it settled.
                        if job.is_final {
                            if !cancel.is_cancelled() {
                                emit(&hub, &id, "final", job.segment, "");
                            }
                            segments.push(String::new());
                            last_error = Some(e);
                        }
                    }
                }
            }
            (segments, last_error)
        })
    };

    // ---- the capture loop --------------------------------------------------
    let mut endpointer = Endpointer::new(END_SILENCE_MS).with_max_utterance(MAX_UTTERANCE_MS);
    let mut segment = 0u32;
    let mut peak_db = -90.0f32;
    let mut frames = 0u64;
    let mut mic_died = false;
    let mut next_partial = Instant::now();
    let deadline = tokio::time::sleep(MAX_SESSION);
    tokio::pin!(deadline);

    let queue_final = |segment: &mut u32, pcm: Vec<u8>| {
        inflight.fetch_add(1, Ordering::SeqCst);
        let _ = jobs.send(Job {
            segment: *segment,
            pcm,
            is_final: true,
        });
        *segment += 1;
    };

    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => break,
            _ = stop.cancelled() => break,
            _ = &mut deadline => break,
            frame = mic.recv() => frame,
        };
        let Some(frame) = frame else {
            mic_died = true;
            break;
        };
        frames += 1;
        peak_db = peak_db.max(stt::dbfs(&frame));

        match endpointer.push(&frame) {
            Endpoint::Quiet => {}
            Endpoint::Started => next_partial = Instant::now() + FIRST_PARTIAL_AFTER,
            Endpoint::Utterance(pcm) => queue_final(&mut segment, pcm),
        }

        // One guess in flight at a time: if the model is still busy with the
        // last one, a newer guess would only wait behind it and arrive stale.
        if partials.load(Ordering::Relaxed)
            && Instant::now() >= next_partial
            && inflight.load(Ordering::SeqCst) == 0
        {
            if let Some(pcm) = endpointer.pending() {
                next_partial = Instant::now() + PARTIAL_EVERY;
                inflight.fetch_add(1, Ordering::SeqCst);
                let _ = jobs.send(Job {
                    segment,
                    pcm: pcm.to_vec(),
                    is_final: false,
                });
            }
        }
    }

    let cancelled = cancel.is_cancelled();
    if !cancelled {
        // Stop was pressed mid-sentence: take what the mic had already handed
        // over, then close the open sentence without waiting for a pause.
        while let Ok(frame) = mic.try_recv() {
            if frame.len() == FRAME_BYTES {
                if let Endpoint::Utterance(pcm) = endpointer.push(&frame) {
                    queue_final(&mut segment, pcm);
                }
            }
        }
        if let Some(pcm) = endpointer.take_pending() {
            queue_final(&mut segment, pcm);
        }
    }
    drop(jobs);
    let _ = ffmpeg.kill().await;
    drop(mic);

    let (segments, last_error) = worker
        .await
        .map_err(|e| anyhow!("dictation transcriber stopped unexpectedly: {e}"))?;
    let _ = std::fs::remove_dir_all(&scratch);

    // The model stays warm for the next dictation, then lets go of its memory.
    session::schedule_sidecar_reap(&voice);

    if cancelled {
        return Ok(Outcome::default());
    }

    let text = segments
        .iter()
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        if let Some(e) = last_error {
            return Err(e);
        }
        if mic_died && frames == 0 {
            return Err(anyhow!(
                "the microphone could not be opened. Check that it is plugged in and not in use by another app, or set THREADKNOT_MIC_DEVICE to the input ffmpeg lists."
            ));
        }
        // Silence this deep is a dead input, and saying "nothing was said"
        // would send the user off to speak louder at a mic that cannot hear.
        let heard_ms = frames * FRAME_MS as u64;
        if heard_ms >= 500 && peak_db < DEAD_MIC_DBFS {
            return Err(anyhow!(
                "the microphone is sending pure silence. It may be muted, or it may not be the input you are speaking into. Check the default recording device in your sound settings, or set THREADKNOT_MIC_DEVICE to the right one."
            ));
        }
    }
    Ok(Outcome { text, segments })
}

#[cfg(test)]
mod tests {
    use super::{prompt_tail, PROMPT_CHARS};

    #[test]
    fn no_context_before_anything_is_settled() {
        assert_eq!(prompt_tail(&[]), None);
        assert_eq!(prompt_tail(&[String::new(), String::new()]), None);
    }

    #[test]
    fn short_context_is_passed_whole_and_skips_empty_segments() {
        let segments = vec!["Open the file.".to_string(), String::new(), "Then run it.".to_string()];
        assert_eq!(prompt_tail(&segments).as_deref(), Some("Open the file. Then run it."));
    }

    #[test]
    fn long_context_keeps_the_tail_and_starts_on_a_whole_word() {
        let segments = vec!["alpha bravo charlie delta echo ".repeat(20)];
        let tail = prompt_tail(&segments).expect("there is context");
        assert!(tail.chars().count() <= PROMPT_CHARS);
        let first = tail.split(' ').next().unwrap();
        assert!(
            ["alpha", "bravo", "charlie", "delta", "echo"].contains(&first),
            "tail must not open mid-word, got {first:?}"
        );
    }
}
