//! Reusable ordered-stream session handles.
//!
//! Ordered Zakura streams are long-lived, peer-owned sessions: a service gets
//! one receive half, one bounded send half, and the peer cancellation token.

use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{Frame, FramedRecv, FramedSend};
use crate::{zakura::ZakuraPeerId, BoxError};

/// A negotiated ordered stream session for one peer and one stream kind.
#[derive(Debug)]
pub struct PeerStreamSession {
    peer_id: ZakuraPeerId,
    stream_kind: u16,
    recv: FramedRecv,
    send: FramedSend,
    cancel_token: CancellationToken,
}

impl PeerStreamSession {
    /// Build a peer-owned ordered stream session.
    pub fn new(
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        recv: FramedRecv,
        send: FramedSend,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            peer_id,
            stream_kind,
            recv,
            send,
            cancel_token,
        }
    }

    /// Authenticated peer identity for this session.
    pub fn peer_id(&self) -> &ZakuraPeerId {
        &self.peer_id
    }

    /// Ordered stream kind owned by this session.
    pub fn stream_kind(&self) -> u16 {
        self.stream_kind
    }

    /// Peer disconnect/local shutdown cancellation token.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Clone the bounded transport send handle.
    pub fn sender(&self) -> FramedSend {
        self.send.clone()
    }

    /// Try to queue a raw frame without awaiting capacity.
    pub fn try_send_frame(&self, frame: Frame) -> Result<(), OrderedSendError> {
        try_send_frame(&self.send, frame)
    }

    /// Encode and try to queue a frame without awaiting capacity.
    pub fn try_send_encoded(
        &self,
        encode: impl FnOnce() -> Result<Frame, BoxError>,
    ) -> Result<(), OrderedSendError> {
        let frame = encode().map_err(OrderedSendError::Encode)?;
        self.try_send_frame(frame)
    }

    /// Split into fields for service-specific session workers.
    pub fn into_parts(self) -> (ZakuraPeerId, u16, FramedRecv, FramedSend, CancellationToken) {
        (
            self.peer_id,
            self.stream_kind,
            self.recv,
            self.send,
            self.cancel_token,
        )
    }
}

/// Nonblocking ordered-stream send failure.
#[derive(Debug, Error)]
pub enum OrderedSendError {
    /// The bounded service-to-transport queue is full.
    #[error("ordered stream send queue is full")]
    Full,

    /// The transport worker for this stream is gone.
    #[error("ordered stream send queue is closed")]
    Closed,

    /// The service-specific frame encoder rejected the message.
    #[error("failed to encode ordered stream frame: {0}")]
    Encode(#[source] BoxError),
}

fn try_send_frame(send: &FramedSend, frame: Frame) -> Result<(), OrderedSendError> {
    match send.try_send(frame) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_frame)) => Err(OrderedSendError::Full),
        Err(mpsc::error::TrySendError::Closed(_frame)) => Err(OrderedSendError::Closed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zakura::framed_channel;

    fn frame(message_type: u16) -> Frame {
        Frame {
            message_type,
            flags: 0,
            payload: Vec::new(),
        }
    }

    #[test]
    fn try_send_succeeds_with_capacity() {
        let (send, _recv) = framed_channel(1);

        assert!(try_send_frame(&send, frame(1)).is_ok());
    }

    #[test]
    fn try_send_returns_full_without_waiting() {
        let (send, _recv) = framed_channel(1);
        try_send_frame(&send, frame(1)).expect("first send has capacity");

        assert!(matches!(
            try_send_frame(&send, frame(2)),
            Err(OrderedSendError::Full)
        ));
    }

    #[test]
    fn try_send_returns_closed_when_worker_is_gone() {
        let (send, recv) = framed_channel(1);
        drop(recv);

        assert!(matches!(
            try_send_frame(&send, frame(1)),
            Err(OrderedSendError::Closed)
        ));
    }
}
