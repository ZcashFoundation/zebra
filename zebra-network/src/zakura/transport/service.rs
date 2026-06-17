//! Zakura protocol service trait surface.

use std::{collections::HashMap, fmt, future::Future, net::IpAddr, pin::Pin};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{FramedRecv, FramedSend};
use crate::{zakura::ZakuraPeerId, BoxError};

use super::Frame;

/// Boxed future returned by object-safe stream handlers.
pub type BoxRunFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Transport mode for a service-declared stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StreamMode {
    /// A long-lived ordered stream between connected peers.
    Ordered,
    /// A short-lived request/response stream opened per request.
    RequestResponse,
}

/// A service-declared Zakura stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Stream {
    /// Unique stream kind carried in `StreamPrelude.stream_kind`.
    pub kind: u16,
    /// Version of this stream kind.
    pub version: u16,
    /// Maximum application frame bytes for this stream.
    pub frame_cap: u32,
    /// Capability bit both peers must negotiate before this stream is wired.
    pub capability: u64,
    /// Stream lifetime and opening semantics.
    pub mode: StreamMode,
}

/// Per-peer transport state handed to a service when a peer connects.
#[derive(Debug)]
pub struct Peer {
    /// Authenticated Zakura peer identity.
    pub id: ZakuraPeerId,
    /// Remote IP address when the transport knows it.
    pub remote_ip: Option<IpAddr>,
    /// Capabilities accepted by both peers.
    pub negotiated: u64,
    streams: HashMap<u16, (FramedRecv, FramedSend)>,
    cancel_token: CancellationToken,
}

impl Peer {
    /// Build a peer from already-opened transport streams.
    pub fn new(
        id: ZakuraPeerId,
        remote_ip: Option<IpAddr>,
        negotiated: u64,
        streams: HashMap<u16, (FramedRecv, FramedSend)>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            id,
            remote_ip,
            negotiated,
            streams,
            cancel_token,
        }
    }

    /// Take ownership of a stream pair for `kind`.
    pub fn take_stream(&mut self, kind: u16) -> Option<(FramedRecv, FramedSend)> {
        self.streams.remove(&kind)
    }

    /// Return the cancellation token for this peer's service tasks.
    ///
    /// The token is the transport supervisor's per-peer disconnect token, so it
    /// fires when this peer disconnects or the local node shuts down.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Split this peer into fields so the registry can fan streams out by owner.
    pub(crate) fn into_parts(
        self,
    ) -> (
        ZakuraPeerId,
        Option<IpAddr>,
        u64,
        HashMap<u16, (FramedRecv, FramedSend)>,
        CancellationToken,
    ) {
        (
            self.id,
            self.remote_ip,
            self.negotiated,
            self.streams,
            self.cancel_token,
        )
    }
}

/// A Zakura protocol service.
pub trait Service: fmt::Debug + Send + Sync + 'static {
    /// Stable service name for logs and diagnostics.
    fn name(&self) -> &'static str;

    /// Streams this service owns.
    fn streams(&self) -> &[Stream];

    /// Add a connected peer and spawn any per-stream work owned by this service.
    fn add_peer(&self, peer: Peer);

    /// Remove a disconnected peer.
    fn remove_peer(&self, peer: &ZakuraPeerId);

    /// Deliver one request-response frame to this service.
    fn deliver_frame(
        &self,
        _peer_id: ZakuraPeerId,
        _stream_kind: u16,
        _frame: Frame,
    ) -> Result<(), SinkReject> {
        Err(SinkReject::protocol(
            "service does not accept inbound frames",
        ))
    }

    /// Deliver one request-response request frame to this service.
    fn request_frame<'a>(
        &'a self,
        _peer_id: ZakuraPeerId,
        _stream_kind: u16,
        _request_id: u64,
        _max_frame_bytes: u32,
        _frame: Frame,
    ) -> BoxRunFuture<'a, Result<Vec<Frame>, SinkReject>> {
        Box::pin(async {
            Err(SinkReject::protocol(
                "service does not accept request frames",
            ))
        })
    }
}

/// A per-stream reader owned by a service.
///
/// This trait deliberately uses an explicit boxed future instead of native
/// `async fn` or the `async-trait` crate: stream handlers are intended to be
/// object-dispatched, and the explicit signature keeps that object safety without
/// adding another dependency.
pub trait Sink: Send + 'static {
    /// Run the reader until the stream closes or is rejected.
    fn run(self: Box<Self>, recv: FramedRecv) -> BoxRunFuture<'static, Result<(), SinkReject>>;
}

/// A per-stream writer owned by a service.
///
/// P2 services can either run this task shape directly or keep a concrete
/// typed send handle built from the [`FramedSend`] handed to [`Service::add_peer`].
///
/// See [`Sink`] for why this uses an explicit boxed future.
pub trait Source: Send + 'static {
    /// Run the writer until the stream closes.
    fn run(self: Box<Self>, send: FramedSend) -> BoxRunFuture<'static, ()>;
}

/// Reason a service sink rejected a decoded frame stream.
#[derive(Debug, Error)]
pub enum SinkReject {
    /// The peer sent protocol-invalid data, so the connection should close.
    #[error("inbound sink rejected protocol-invalid frame: {0}")]
    Protocol(#[source] BoxError),

    /// Local sink state prevented delivery; the peer is not at fault.
    #[error("inbound sink could not accept frame locally: {0}")]
    Local(#[source] BoxError),
}

impl SinkReject {
    /// Build a fatal peer-protocol rejection.
    pub fn protocol(error: impl Into<BoxError>) -> Self {
        Self::Protocol(error.into())
    }

    /// Build a non-fatal local-delivery rejection.
    pub fn local(error: impl Into<BoxError>) -> Self {
        Self::Local(error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_reject_constructors_preserve_protocol_and_local_contract() {
        let protocol = SinkReject::protocol("bad frame");
        let local = SinkReject::local("closed queue");

        assert!(matches!(protocol, SinkReject::Protocol(_)));
        assert!(matches!(local, SinkReject::Local(_)));
        assert!(protocol.to_string().contains("protocol-invalid"));
        assert!(local.to_string().contains("locally"));
    }
}
