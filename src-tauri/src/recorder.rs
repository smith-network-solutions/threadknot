//! Screen recording for the agent browser.
//!
//! The CDP screencast is a *variable rate* source: Chrome emits a frame when it
//! repaints and nothing at all while the page is idle. Piping that straight into
//! a video yields frozen stretches and jerky motion, because the encoder has no
//! way to know a 900ms gap was a deliberate pause rather than a dropped frame.
//!
//! So the recorder resamples. It subscribes to the session's existing frame
//! broadcast (no change to the screencast path), keeps the most recent JPEG, and
//! a fixed-rate ticker writes that JPEG to ffmpeg once per output frame —
//! repeating the last one through idle stretches. The result is constant frame
//! rate video where a pause reads as a pause.
//!
//! Frames go to ffmpeg as an MJPEG stream on stdin, so we never decode a JPEG in
//! this process; ffmpeg does it once on its way to H.264.

use crate::protocol::ArtifactRecord;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::broadcast;

/// Output frames per second. 30 is smooth for pointer motion without making the
/// file large; the source rarely exceeds it.
const FPS: u32 = 30;

/// Give up waiting for the first frame rather than record a black video.
const FIRST_FRAME_TIMEOUT_MS: u64 = 4_000;

/// A recording in progress.
pub struct Recording {
    path: PathBuf,
    ffmpeg: Child,
    pump: tokio::task::JoinHandle<()>,
    /// Frames actually written, for the duration report.
    written: Arc<Mutex<u64>>,
}

impl Recording {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// True when a usable ffmpeg is on PATH. Recording is the only feature that
/// needs it, so its absence must degrade to a clear message rather than a panic.
pub fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Begin encoding `frames` to `path`.
///
/// `seed` is the session's most recent frame; without it a recording started
/// during an idle page would wait for the next repaint before producing
/// anything, and short flows can finish before that ever happens.
pub async fn start(
    path: PathBuf,
    mut frames: broadcast::Receiver<Vec<u8>>,
    seed: Option<Vec<u8>>,
) -> Result<Recording> {
    if !ffmpeg_available() {
        return Err(anyhow!(
            "ffmpeg is required to record the browser but was not found on PATH"
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            // Input: our resampled MJPEG stream, already at the output rate.
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "-framerate",
            &FPS.to_string(),
            "-i",
            "-",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            // Screencast dimensions are even by construction, but a resize mid
            // capture could produce an odd edge; this keeps yuv420p legal.
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",
            // yuv420p + faststart = plays in every browser and embeds in docs.
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start ffmpeg")?;

    let mut stdin = ffmpeg
        .stdin
        .take()
        .ok_or_else(|| anyhow!("ffmpeg stdin unavailable"))?;

    let written = Arc::new(Mutex::new(0u64));
    let counter = written.clone();

    let pump = tokio::spawn(async move {
        let mut latest = seed;
        // Wait for something to draw, but not forever.
        if latest.is_none() {
            let deadline =
                tokio::time::Duration::from_millis(FIRST_FRAME_TIMEOUT_MS);
            if let Ok(Ok(first)) = tokio::time::timeout(deadline, frames.recv()).await {
                latest = Some(first);
            }
        }
        let Some(mut current) = latest else {
            return;
        };

        let period = tokio::time::Duration::from_nanos(1_000_000_000 / FPS as u64);
        let mut tick = tokio::time::interval(period);
        // A stalled write must not let ticks pile up and then burst.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Drain every frame that arrives; only the newest is kept, so a
                // burst of repaints between ticks collapses to one output frame.
                received = frames.recv() => {
                    match received {
                        Ok(bytes) => current = bytes,
                        // Lagged means we fell behind the broadcast buffer; the
                        // next recv still yields a recent frame, so keep going.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = tick.tick() => {
                    if stdin.write_all(&current).await.is_err() {
                        break;
                    }
                    *counter.lock().unwrap() += 1;
                }
            }
        }
        let _ = stdin.flush().await;
        let _ = stdin.shutdown().await;
    });

    Ok(Recording {
        path,
        ffmpeg,
        pump,
        written,
    })
}

/// Finish the recording and return the finished file plus its duration.
pub async fn finish(recording: Recording) -> Result<(PathBuf, u64)> {
    let Recording {
        path,
        ffmpeg,
        pump,
        written,
    } = recording;

    // Aborting the pump drops ffmpeg's stdin, which is how ffmpeg learns the
    // stream ended and writes its moov atom.
    pump.abort();
    let _ = pump.await;

    let output = ffmpeg
        .wait_with_output()
        .await
        .context("waiting for ffmpeg")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("ffmpeg failed: {}", err.trim()));
    }
    let frames = *written.lock().unwrap();
    let duration_ms = frames * 1000 / FPS as u64;
    if !path.is_file() {
        return Err(anyhow!("ffmpeg produced no output file"));
    }
    Ok((path, duration_ms))
}

/// Where a recording lands when the caller does not name a path: alongside the
/// session's other outputs, with a sortable name.
pub fn default_output(dir: &Path, stamp: &str) -> PathBuf {
    dir.join(format!("recording-{stamp}.mp4"))
}

/// Convenience for the MCP layer: describe a finished recording.
pub fn describe(path: &Path, duration_ms: u64) -> serde_json::Value {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    serde_json::json!({
        "path": path.display().to_string(),
        "durationMs": duration_ms,
        "sizeBytes": size,
        "mimeType": "video/mp4",
    })
}

/// Marker so `ArtifactRecord` stays referenced if the MCP layer publishes
/// directly; keeps the module's public surface honest about what it returns.
pub type PublishedRecording = Option<ArtifactRecord>;
