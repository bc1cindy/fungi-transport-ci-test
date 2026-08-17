//! Length-prefix framing: adapt any byte stream into a datagram [`Channel`].
//!
//! Wire format: 4-byte big-endian payload length, then the payload — the
//! least machinery that restores message boundaries over a raw byte stream.
//! Kept intentionally minimal because a typed (TLV) format is expected to
//! replace the wire format later, leaving the [`Channel`] interface intact.
//!
//! Cancel safety: all partial-frame state lives in [`FramedChannel`], not in
//! the `recv` future, so a `recv` dropped mid-frame (e.g. by `select!`)
//! resumes exactly where it stopped. `send` is NOT cancel safe: dropping a
//! `send` future mid-write can leave a half-written frame on the stream.

use std::future::Future;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::channel::Channel;
use crate::error::{RecvError, SendError};

/// Default maximum message size (1 MiB).
pub const DEFAULT_MAX_MSG_LEN: usize = 1024 * 1024;

/// A [`Channel`] over any byte stream, delimiting messages with a 4-byte
/// big-endian length prefix. Generic over the stream so one framing layer
/// serves every byte-stream backend — a tor daemon's `TcpStream`, an
/// in-process arti data stream — instead of each reimplementing framing.
#[derive(Debug)]
pub struct FramedChannel<S> {
    stream: S,
    max_msg_len: usize,
    // Partial-frame read state (cancel safety: lives here, not in futures).
    header: [u8; 4],
    header_filled: usize,
    payload: Vec<u8>,
    payload_filled: usize,
    // Set on protocol violation; every later recv returns Closed.
    poisoned: bool,
}

impl<S> FramedChannel<S> {
    /// Wrap `stream`. `max_msg_len` bounds both directions: larger sends
    /// fail with [`SendError::TooLarge`], larger incoming frames poison the
    /// channel (a peer announcing a huge frame must not induce the
    /// allocation).
    ///
    /// # Panics
    ///
    /// If `max_msg_len` does not fit in the u32 length prefix.
    pub fn new(stream: S, max_msg_len: usize) -> Self {
        assert!(
            u32::try_from(max_msg_len).is_ok(),
            "max_msg_len must fit in the u32 length prefix"
        );
        Self {
            stream,
            max_msg_len,
            header: [0; 4],
            header_filled: 0,
            payload: Vec::new(),
            payload_filled: 0,
            poisoned: false,
        }
    }
}

/// Write-side errors: peer/stream gone maps to `Closed`, the rest is opaque.
fn map_send_io(e: std::io::Error) -> SendError {
    use std::io::ErrorKind::*;
    match e.kind() {
        BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof | WriteZero => {
            SendError::Closed
        }
        _ => SendError::Transport(e.into()),
    }
}

/// Read-side errors: reset means the peer is gone, the rest is opaque.
fn map_recv_io(e: std::io::Error) -> RecvError {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionReset | ConnectionAborted => RecvError::Closed,
        _ => RecvError::Transport(e.into()),
    }
}

impl<S> FramedChannel<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    async fn send_inner(&mut self, msg: &[u8]) -> Result<(), SendError> {
        if msg.len() > self.max_msg_len {
            return Err(SendError::TooLarge {
                max: self.max_msg_len,
            });
        }
        // max_msg_len fits in u32 (checked in new), so msg.len() does too.
        let prefix = (msg.len() as u32).to_be_bytes();
        self.stream.write_all(&prefix).await.map_err(map_send_io)?;
        self.stream.write_all(msg).await.map_err(map_send_io)?;
        self.stream.flush().await.map_err(map_send_io)?;
        Ok(())
    }

    async fn recv_inner(&mut self) -> Result<Vec<u8>, RecvError> {
        if self.poisoned {
            return Err(RecvError::Closed);
        }
        while self.header_filled < 4 {
            let n = self
                .stream
                .read(&mut self.header[self.header_filled..])
                .await
                .map_err(map_recv_io)?;
            if n == 0 {
                // EOF between frames is a clean close; inside one, a
                // truncation.
                if self.header_filled == 0 {
                    return Err(RecvError::Closed);
                }
                self.poisoned = true;
                return Err(RecvError::Transport("stream ended mid-frame".into()));
            }
            self.header_filled += n;
        }
        let len = u32::from_be_bytes(self.header) as usize;
        if len > self.max_msg_len {
            self.poisoned = true;
            return Err(RecvError::Transport(
                format!(
                    "peer announced a {len}-byte frame, exceeding the {}-byte maximum",
                    self.max_msg_len
                )
                .into(),
            ));
        }
        // First entry for this frame: payload was taken (empty) after the
        // previous one. On cancel-resume it is already sized.
        if self.payload.len() != len {
            self.payload.resize(len, 0);
        }
        while self.payload_filled < len {
            let n = self
                .stream
                .read(&mut self.payload[self.payload_filled..])
                .await
                .map_err(map_recv_io)?;
            if n == 0 {
                self.poisoned = true;
                return Err(RecvError::Transport("stream ended mid-frame".into()));
            }
            self.payload_filled += n;
        }
        self.header_filled = 0;
        self.payload_filled = 0;
        Ok(std::mem::take(&mut self.payload))
    }
}

impl<S> Channel for FramedChannel<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    fn send(&mut self, msg: &[u8]) -> impl Future<Output = Result<(), SendError>> + Send {
        self.send_inner(msg)
    }

    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, RecvError>> + Send {
        self.recv_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    /// A connected pair of framed channels over an in-process duplex pipe.
    fn framed_pair(
        max: usize,
    ) -> (
        FramedChannel<tokio::io::DuplexStream>,
        FramedChannel<tokio::io::DuplexStream>,
    ) {
        // 64 KiB pipe buffer: big enough that small test frames never
        // deadlock on unread data, small enough to exercise partial writes.
        let (a, b) = tokio::io::duplex(64 * 1024);
        (FramedChannel::new(a, max), FramedChannel::new(b, max))
    }

    #[tokio::test]
    async fn roundtrip_both_directions() {
        let (a, b) = framed_pair(DEFAULT_MAX_MSG_LEN);
        testkit::roundtrip_both_directions(a, b).await;
    }

    #[tokio::test]
    async fn too_large_is_rejected() {
        let (a, _b) = framed_pair(16);
        testkit::too_large(a, 16).await;
    }

    #[tokio::test]
    async fn recv_after_peer_drop_is_closed() {
        let (a, b) = framed_pair(DEFAULT_MAX_MSG_LEN);
        testkit::closed_after_peer_drop(a, b).await;
    }

    #[tokio::test]
    async fn recv_is_cancel_safe() {
        let (a, b) = framed_pair(DEFAULT_MAX_MSG_LEN);
        testkit::recv_is_cancel_safe(a, b).await;
    }
}
