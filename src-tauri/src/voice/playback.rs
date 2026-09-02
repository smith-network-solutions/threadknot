//! Audio playback through ffplay.
//!
//! The webview cannot play this audio (CSP: no external origins, no blob
//! media), and this machine's speakers belong to the same box as its mic — so
//! playback is a child process, exactly the shell-out idiom capture already
//! uses for ffmpeg. One ffplay per assistant reply: closing stdin ends it and
//! `wait()` exiting IS "playback finished", which drives the speaking →
//! listening transition precisely. Barge-in is `kill()` — audio stops within
//! the OS buffer, no pacing logic.
//!
//! V2's phone path adds a webview sink behind the same surface; nothing in
//! the session loop should care which sink it feeds.

use anyhow::{Context, Result};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};

/// Sample rate encoded in a `pcm_*` output format ("pcm_22050" → 22050).
/// `None` means the format is a container ffplay can sniff (mp3, opus).
pub fn pcm_sample_rate(output_format: &str) -> Option<u32> {
    output_format.strip_prefix("pcm_")?.parse().ok()
}

pub struct FfplaySink {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl FfplaySink {
    /// Start an ffplay reading audio from stdin. `pcm_rate` describes raw
    /// s16le mono at that rate; `None` lets ffplay detect a container format.
    pub fn spawn(pcm_rate: Option<u32>) -> Result<Self> {
        let bin = crate::dictation::tool("ffplay")?;
        let mut cmd = Command::new(bin);
        cmd.args(["-hide_banner", "-loglevel", "error", "-nodisp", "-autoexit"]);
        if let Some(rate) = pcm_rate {
            cmd.args(["-f", "s16le", "-ar", &rate.to_string(), "-ch_layout", "mono"]);
        }
        cmd.args(["-i", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::agents::no_console(&mut cmd);
        let mut child = cmd
            .spawn()
            .context("could not start ffplay to play voice audio")?;
        let stdin = child.stdin.take();
        Ok(Self { child, stdin })
    }

    pub async fn write(&mut self, audio: &[u8]) -> Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            stdin
                .write_all(audio)
                .await
                .context("audio playback closed early")?;
        }
        Ok(())
    }

    /// No more audio for this reply: close stdin and wait for the buffered
    /// tail to finish playing. Resolving IS "the room is quiet again".
    pub async fn finish(mut self) -> Result<()> {
        drop(self.stdin.take());
        // Bounded: -autoexit ends ffplay when the stream drains; the timeout
        // only guards a wedged child.
        match tokio::time::timeout(Duration::from_secs(60), self.child.wait()).await {
            Ok(_) => Ok(()),
            Err(_) => {
                let _ = self.child.kill().await;
                Ok(())
            }
        }
    }

    /// Barge-in / cancel: stop the sound now.
    pub async fn kill(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill().await;
    }
}

/// Pump an HTTP audio stream into a sink until it drains or `cancel` fires.
/// Returns whether the stream completed (false = cancelled).
pub async fn pump(
    response: reqwest::Response,
    sink: &mut FfplaySink,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<bool> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                sink.kill().await;
                return Ok(false);
            }
            next = stream.next() => match next {
                Some(bytes) => {
                    let bytes = bytes.context("the audio stream broke off")?;
                    sink.write(&bytes).await?;
                }
                None => return Ok(true),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pcm_sample_rate;

    #[test]
    fn pcm_formats_carry_their_rate() {
        assert_eq!(pcm_sample_rate("pcm_22050"), Some(22050));
        assert_eq!(pcm_sample_rate("pcm_16000"), Some(16000));
        assert_eq!(pcm_sample_rate("mp3_44100_128"), None);
        assert_eq!(pcm_sample_rate("pcm_x"), None);
    }
}
