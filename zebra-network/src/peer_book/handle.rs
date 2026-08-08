//! A cloneable [`tower::Service`] handle for peer book requests, and the
//! change sender feeding the actor.

use std::task::{Context, Poll};

use futures::future::BoxFuture;
use tokio::sync::mpsc::error::{SendError, TrySendError};
use tokio_util::sync::PollSender;
use tower::Service;

use crate::{
    meta_addr::{MetaAddr, MetaAddrChange},
    peer_book::transports::AddrTransports,
    BoxError,
};

/// A message to the peer book actor.
///
/// Changes and calls travel on one bounded channel, so the order between a
/// connection event and a later request is preserved, and backpressure is
/// defined in one place.
#[derive(Debug)]
pub(crate) enum Message {
    /// A peer status change, applied in channel order.
    Change(MetaAddrChange),

    /// A request that needs an answer from the book.
    Call(Call),

    /// A learned transport reachability report, applied in channel order.
    Transport {
        /// The peer's address.
        addr: crate::PeerSocketAddr,

        /// The transport the dial used.
        transport: AddrTransports,

        /// Whether the handshake completed.
        reachable: bool,
    },
}

/// Sends peer status changes to the peer book actor.
///
/// This preserves the sender API that connections and handshakes already
/// use: changes are sent losslessly with [`send`](Self::send), and applied
/// by the actor in channel order.
#[derive(Clone, Debug)]
pub struct ChangeSender {
    /// Sends changes to the actor.
    tx: tokio::sync::mpsc::Sender<Message>,
}

impl ChangeSender {
    /// Creates a change sender over the actor's message channel.
    pub(crate) fn new(tx: tokio::sync::mpsc::Sender<Message>) -> Self {
        ChangeSender { tx }
    }

    /// Sends `change` to the actor, waiting for channel capacity.
    ///
    /// Returns an error only if the actor has exited.
    pub async fn send(&self, change: MetaAddrChange) -> Result<(), SendError<MetaAddrChange>> {
        self.tx
            .send(Message::Change(change))
            .await
            .map_err(|SendError(message)| match message {
                Message::Change(change) => SendError(change),
                _ => unreachable!("this method only sends changes"),
            })
    }

    /// Reports that a dial to `addr` over `transport` completed its
    /// handshake, or was refused.
    ///
    /// Reports are best-effort: a full channel means the actor is busy
    /// applying connection events, and reachability is re-learned on the
    /// next dial.
    pub fn record_transport(
        &self,
        addr: crate::PeerSocketAddr,
        transport: AddrTransports,
        reachable: bool,
    ) {
        let _ = self.tx.try_send(Message::Transport {
            addr,
            transport,
            reachable,
        });
    }

    /// Sends `change` to the actor without waiting.
    ///
    /// Returns an error if the channel is full or the actor has exited.
    pub fn try_send(&self, change: MetaAddrChange) -> Result<(), TrySendError<MetaAddrChange>> {
        self.tx
            .try_send(Message::Change(change))
            .map_err(|error| match error {
                TrySendError::Full(Message::Change(change)) => TrySendError::Full(change),
                TrySendError::Closed(Message::Change(change)) => TrySendError::Closed(change),
                TrySendError::Full(_) | TrySendError::Closed(_) => {
                    unreachable!("this method only sends changes")
                }
            })
    }
}

/// A request to the peer book actor.
#[derive(Clone, Debug)]
pub enum PeerBookRequest {
    /// Requests the sanitized addresses served to peers that ask for
    /// addresses.
    SanitizedAddrs,

    /// Requests the addresses worth writing to the peer cache on disk.
    CacheSnapshot,

    /// Selects a peer for an outbound connection attempt, marking it as
    /// attempted in the same actor turn, so concurrent selections cannot
    /// return the same peer.
    SelectCandidate,

    /// Requests the number of peers currently ready for an outbound
    /// connection attempt.
    ///
    /// The returned count is a snapshot: candidates can become ready or be
    /// attempted immediately afterwards.
    ReadyCandidateCount,

    /// Adds gossiped peer addresses to the book.
    ///
    /// The actor validates the addresses before storing them, adjusting or
    /// rejecting untrusted last-seen times.
    GossipedAddrs {
        /// The gossiped addresses, as received from a peer.
        addrs: Vec<MetaAddr>,
    },

    /// Test-only: overrides the book's local listener address.
    #[cfg(any(test, feature = "proptest-impl"))]
    TestSetLocalListener(std::net::SocketAddr),

    /// Test-only: returns the book's freshest entry for a peer.
    #[cfg(any(test, feature = "proptest-impl"))]
    TestPeerEntry(crate::PeerSocketAddr),
}

/// A response from the peer book actor.
#[derive(Clone, Debug)]
pub enum PeerBookResponse {
    /// The requested addresses.
    Addrs(Vec<MetaAddr>),

    /// The selected outbound connection candidate, marked as attempted,
    /// with the transports it is known to accept.
    Candidate(Option<(MetaAddr, AddrTransports)>),

    /// The number of peers ready for an outbound connection attempt.
    ReadyCandidateCount(usize),

    /// The request was applied, and has no data to return.
    Done,

    /// Test-only: the book's freshest entry for a peer, if any.
    #[cfg(any(test, feature = "proptest-impl"))]
    PeerEntry(Option<MetaAddr>),
}

/// A call in flight to the actor: the request and its reply channel.
#[derive(Debug)]
pub(crate) struct Call {
    /// The request to answer.
    pub(crate) request: PeerBookRequest,

    /// The channel the response is sent on. Dropping it cancels the call.
    pub(crate) reply: tokio::sync::oneshot::Sender<Result<PeerBookResponse, BoxError>>,
}

/// A cloneable handle that sends requests to the peer book actor.
///
/// `poll_ready` reserves a channel slot, so the handle never reports
/// readiness without the capacity to send, and backpressure from the actor
/// propagates to callers.
#[derive(Debug)]
pub struct PeerBookHandle {
    /// Sends calls to the actor, reserving capacity in `poll_ready`.
    tx: PollSender<Message>,
}

impl Clone for PeerBookHandle {
    fn clone(&self) -> Self {
        PeerBookHandle {
            tx: self.tx.clone(),
        }
    }
}

impl PeerBookHandle {
    /// Creates a handle sending calls on `tx`.
    pub(crate) fn new(tx: tokio::sync::mpsc::Sender<Message>) -> Self {
        PeerBookHandle {
            tx: PollSender::new(tx),
        }
    }

    /// Spawns a peer book actor owning `address_book`, and returns a handle
    /// and change sender for it, for tests that construct address books
    /// directly.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn spawn_for_book(address_book: crate::AddressBook) -> (Self, ChangeSender) {
        let local_listener = address_book.local_listener_socket_addr();
        let handles = crate::address_book_updater::AddressBookUpdater::spawn_with_book(
            address_book,
            local_listener,
            100,
        );

        (handles.handle, handles.change_sender)
    }
}

impl Service<PeerBookRequest> for PeerBookHandle {
    type Response = PeerBookResponse;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<PeerBookResponse, BoxError>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.tx
            .poll_reserve(cx)
            .map_err(|_closed| "the peer book actor has exited".into())
    }

    fn call(&mut self, request: PeerBookRequest) -> Self::Future {
        let (reply, response_rx) = tokio::sync::oneshot::channel();

        // The slot was reserved in `poll_ready`, so this only fails if the
        // actor has exited.
        let sent = self.tx.send_item(Message::Call(Call { request, reply }));

        Box::pin(async move {
            sent.map_err(|_closed| BoxError::from("the peer book actor has exited"))?;

            response_rx
                .await
                .map_err(|_dropped| BoxError::from("the peer book actor dropped the request"))?
        })
    }
}
