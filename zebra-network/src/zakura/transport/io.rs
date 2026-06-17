//! Transport-owned framed stream handles.
//!
//! `FramedRecv` and `FramedSend` are the service-facing handles for application
//! stream frames. The transport applies the authoritative per-kind cap from
//! `app_frame_cap_for_stream_kind`, per-kind message-rate buckets, and idle
//! freshness updates in its stream workers before frames reach these handles.

use tokio::sync::mpsc;

use super::Frame;

/// Receive half for bounded, rate-admitted Zakura frames.
#[derive(Debug)]
pub struct FramedRecv {
    receiver: mpsc::Receiver<Frame>,
}

impl FramedRecv {
    /// Wrap a bounded frame receiver.
    pub fn new(receiver: mpsc::Receiver<Frame>) -> Self {
        Self { receiver }
    }

    /// Receive the next admitted frame, or `None` after the transport closes the stream.
    pub async fn recv(&mut self) -> Option<Frame> {
        self.receiver.recv().await
    }
}

/// Send half for bounded Zakura frames.
#[derive(Clone, Debug)]
pub struct FramedSend {
    sender: mpsc::Sender<Frame>,
}

impl FramedSend {
    /// Wrap a bounded frame sender.
    pub fn new(sender: mpsc::Sender<Frame>) -> Self {
        Self { sender }
    }

    /// Queue a frame for transport-owned encoding and writing.
    pub async fn send(&self, frame: Frame) -> Result<(), mpsc::error::SendError<Frame>> {
        self.sender.send(frame).await
    }

    /// Try to queue a frame without waiting for capacity.
    pub fn try_send(&self, frame: Frame) -> Result<(), mpsc::error::TrySendError<Frame>> {
        self.sender.try_send(frame)
    }
}

/// Build a bounded in-memory framed channel for scaffolding and tests.
pub fn framed_channel(depth: usize) -> (FramedSend, FramedRecv) {
    let (sender, receiver) = mpsc::channel(depth);
    (FramedSend::new(sender), FramedRecv::new(receiver))
}
