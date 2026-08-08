//! Length-prefixed JSON framing for the handshake and the control stream.
//!
//! Deliberately boring: a `u32` big-endian length, then that many bytes of JSON.
//! The handshake is a handful of small messages, so there is nothing to gain
//! from a binary encoding and everything to gain from being able to read a
//! packet capture during an incident.
//!
//! The length cap is the important part. Without it, a hostile peer sends
//! `0xFFFFFFFF` and the reader allocates 4 GB before it has authenticated
//! anything — a one-packet OOM on either side.

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame either side will send or accept. Handshake and control frames
/// are a few hundred bytes; the routing table is fetched over HTTP, not here.
pub const MAX_FRAME: u32 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(u32),
    #[error("malformed frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// Write one length-prefixed JSON frame.
pub async fn write_frame<W, T>(w: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    let len = u32::try_from(bytes.len()).map_err(|_| FrameError::TooLarge(u32::MAX))?;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    // One write for the header and one for the body is two syscalls and, worse,
    // two TCP segments for a 200-byte handshake message. Build it once.
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&bytes);
    w.write_all(&out).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed JSON frame.
pub async fn read_frame<R, T>(r: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0u8; 4];
    r.read_exact(&mut header).await?;
    let len = u32::from_be_bytes(header);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips() {
        let hello = crate::ConnectorHello {
            protocol_version: crate::PROTOCOL_VERSION,
            installation_id: "inst-1".into(),
            public_key: "AAAA".into(),
            client_version: Some("0.1.99".into()),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &hello).await.unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let back: crate::ConnectorHello = read_frame(&mut cursor).await.unwrap();
        assert_eq!(back, hello);
    }

    #[tokio::test]
    async fn an_oversized_length_header_is_refused_before_allocating() {
        // The whole point of the cap: this must fail on the header, not after
        // reserving 4 GB for a body that will never arrive.
        let mut cursor = std::io::Cursor::new(u32::MAX.to_be_bytes().to_vec());
        let err = read_frame::<_, serde_json::Value>(&mut cursor).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn a_truncated_frame_is_an_error_not_a_hang() {
        let mut cursor = std::io::Cursor::new(vec![0, 0, 0, 8, b'{']);
        assert!(read_frame::<_, serde_json::Value>(&mut cursor).await.is_err());
    }
}
