//! A [`tower::Service`] front-end for the [`AddressBook`].
//!
//! The address book is a single shared piece of state guarded by a blocking
//! [`std::sync::Mutex`]. Wrapping it in a `Service` gives us a clone-able,
//! request/response oriented handle, hides the locking strategy from callers,
//! and lets us run all address book operations on a blocking thread (matching
//! the historical "spawn_blocking before locking" pattern used everywhere this
//! mutex was held).
//!
//! See [`AddressBookRequest`] for the supported operations.

use std::{
    fmt,
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex, TryLockError},
    task::{Context, Poll},
    time::Instant,
};

use chrono::Utc;
use futures::FutureExt;
use indexmap::IndexMap;
use tokio::sync::{mpsc, watch};
use tower::Service;
use tracing::Span;

use crate::{
    address_book::AddressMetrics, meta_addr::MetaAddrChange, types::MetaAddr, AddressBook,
    AddressBookPeers, BoxError, PeerAddrState, PeerSocketAddr,
};

#[cfg(test)]
mod tests;

/// Operations supported by [`AddressBookService`].
///
/// Each variant maps directly to a single method on [`AddressBook`]; see those
/// methods for the precise semantics.
#[derive(Clone, Debug)]
pub enum AddressBookRequest {
    /// Look up `addr` in the address book.
    Get(PeerSocketAddr),

    /// Apply a single change to the address book.
    Update(MetaAddrChange),

    /// Apply many changes to the address book in a single critical section.
    Extend(Vec<MetaAddrChange>),

    /// Add `addr` to the address book in the initial-peer state, returning
    /// `true` if it was newly inserted.
    AddPeer(PeerSocketAddr),

    /// All peers, in reconnection-attempt order.
    Peers,

    /// Peers in `state`, in reconnection-attempt order.
    StatePeers(PeerAddrState),

    /// `Responded` peers that are still considered live.
    RecentlyLivePeers,

    /// All peers eligible for an outbound connection attempt right now.
    ReconnectionPeers,

    /// Peers that may currently be connected.
    MaybeConnectedPeers,

    /// A randomized + sanitized `getaddr` response, including the local
    /// listener address.
    FreshGetAddrResponse,

    /// Like [`FreshGetAddrResponse`](Self::FreshGetAddrResponse), but returns
    /// `MaybePeers(None)` instead of blocking when the address book lock is
    /// contended.
    TryFreshGetAddrResponse,

    /// Peers in preferred caching order, excluding the local listener.
    Cacheable,

    /// The local listener as a [`MetaAddr`].
    LocalListenerMetaAddr,

    /// The local listener [`SocketAddr`].
    LocalListenerSocketAddr,

    /// Atomically pick the next peer eligible for a reconnection attempt and
    /// transition it to `AttemptPending`.
    NextReconnectionPeer,

    /// The currently-banned IPs.
    Bans,

    /// Number of addresses currently in the address book.
    Len,

    /// Whether `addr` is currently pending a reconnection attempt.
    PendingReconnectionAddr(PeerSocketAddr),
}

/// Responses from [`AddressBookService`].
///
/// The variant returned for a given request is documented on each
/// [`AddressBookRequest`] variant.
#[derive(Clone, Debug)]
pub enum AddressBookResponse {
    /// Returned by `Get`, `Update`, and `NextReconnectionPeer`. `None` means
    /// the address was not present, or no peer was eligible.
    MaybePeer(Option<MetaAddr>),

    /// Returned by all multi-peer queries.
    Peers(Vec<MetaAddr>),

    /// Returned by [`AddressBookRequest::TryFreshGetAddrResponse`]: `None`
    /// means the address book lock was contended.
    MaybePeers(Option<Vec<MetaAddr>>),

    /// Returned by [`AddressBookRequest::LocalListenerMetaAddr`].
    MetaAddr(MetaAddr),

    /// Returned by [`AddressBookRequest::LocalListenerSocketAddr`].
    SocketAddr(SocketAddr),

    /// Returned by `AddPeer` and `PendingReconnectionAddr`.
    Bool(bool),

    /// Returned by [`AddressBookRequest::Len`].
    Count(usize),

    /// Returned by [`AddressBookRequest::Bans`].
    Bans(Arc<IndexMap<IpAddr, Instant>>),

    /// Returned by `Extend`.
    Unit,
}

/// A clone-able [`tower::Service`] handle to an [`AddressBook`].
///
/// All operations are performed on a blocking thread via
/// [`tokio::task::spawn_blocking`], so callers never block the runtime on the
/// address book mutex. The service also exposes the watch channels that other
/// components subscribe to (address metrics and bans), and the
/// [`mpsc::Sender`] used by the misbehavior-batching task to feed changes to
/// the underlying [`crate::address_book_updater::AddressBookUpdater`] worker.
#[derive(Clone)]
pub struct AddressBookService {
    inner: Arc<Mutex<AddressBook>>,
    metrics_rx: watch::Receiver<AddressMetrics>,
    bans_rx: watch::Receiver<Arc<IndexMap<IpAddr, Instant>>>,
    update_tx: mpsc::Sender<MetaAddrChange>,
    span: Span,
}

impl fmt::Debug for AddressBookService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AddressBookService")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl AddressBookService {
    /// Wrap the given primitives in an [`AddressBookService`].
    ///
    /// Production code should call
    /// [`AddressBookUpdater::spawn`](crate::address_book_updater::AddressBookUpdater::spawn)
    /// instead, which constructs both the service and its background update
    /// worker.
    pub(crate) fn new(
        inner: Arc<Mutex<AddressBook>>,
        metrics_rx: watch::Receiver<AddressMetrics>,
        bans_rx: watch::Receiver<Arc<IndexMap<IpAddr, Instant>>>,
        update_tx: mpsc::Sender<MetaAddrChange>,
        span: Span,
    ) -> Self {
        Self {
            inner,
            metrics_rx,
            bans_rx,
            update_tx,
            span,
        }
    }

    /// Watch channel of the latest [`AddressMetrics`] snapshot.
    pub fn metrics_watcher(&self) -> watch::Receiver<AddressMetrics> {
        self.metrics_rx.clone()
    }

    /// Watch channel of the currently-banned IPs.
    pub fn bans_watcher(&self) -> watch::Receiver<Arc<IndexMap<IpAddr, Instant>>> {
        self.bans_rx.clone()
    }

    /// Sender for the high-throughput batched update channel feeding the
    /// [`AddressBookUpdater`](crate::address_book_updater::AddressBookUpdater) worker.
    ///
    /// Peer connection tasks send their `MetaAddrChange`s here so the address
    /// book mutex isn't taken on every individual peer event.
    pub fn update_sender(&self) -> mpsc::Sender<MetaAddrChange> {
        self.update_tx.clone()
    }

    /// Direct access to the underlying mutex.
    ///
    /// Provided for the small number of internal sites (and tests) that need
    /// to perform compound operations under a single lock. New code should
    /// prefer the [`Service`] interface.
    pub fn shared(&self) -> Arc<Mutex<AddressBook>> {
        self.inner.clone()
    }

    /// Wrap an existing [`AddressBook`] in a service for testing.
    ///
    /// The returned service uses freshly-created watch and mpsc channels.
    /// Callers that need access to the update sender or watchers should keep
    /// the service handle and use the corresponding accessor methods.
    pub fn from_book_for_tests(book: AddressBook) -> Self {
        let metrics_rx = book.address_metrics_watcher();
        let bans = book.bans();
        let inner = Arc::new(Mutex::new(book));
        let (_bans_tx, bans_rx) = watch::channel(bans);
        let (update_tx, _update_rx) = mpsc::channel(MIN_TEST_UPDATE_CHANNEL_SIZE);

        // The receivers we drop here would normally be held by the
        // `AddressBookUpdater` worker; in tests we rely on direct mutex access
        // through `shared()`.
        Self {
            inner,
            metrics_rx,
            bans_rx,
            update_tx,
            span: Span::none(),
        }
    }

    /// Wrap an existing `Arc<Mutex<AddressBook>>` in a service for testing.
    pub fn from_shared_for_tests(inner: Arc<Mutex<AddressBook>>) -> Self {
        let (metrics_rx, bans) = {
            let book = inner
                .lock()
                .expect("address book lock poisoned in test setup");
            (book.address_metrics_watcher(), book.bans())
        };
        let (_bans_tx, bans_rx) = watch::channel(bans);
        let (update_tx, _update_rx) = mpsc::channel(MIN_TEST_UPDATE_CHANNEL_SIZE);

        Self {
            inner,
            metrics_rx,
            bans_rx,
            update_tx,
            span: Span::none(),
        }
    }
}

const MIN_TEST_UPDATE_CHANNEL_SIZE: usize = 16;

impl Service<AddressBookRequest> for AddressBookService {
    type Response = AddressBookResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // The address book mutex itself doesn't expose readiness signaling, so
        // there's nothing useful to wait for in `poll_ready`. Backpressure for
        // peer-driven updates is provided by the separate `update_sender`
        // channel.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: AddressBookRequest) -> Self::Future {
        let inner = self.inner.clone();
        let span = self.span.clone();

        async move {
            tokio::task::spawn_blocking(move || span.in_scope(|| handle_request(&inner, req)))
                .await
                .map_err(|join_err| -> BoxError {
                    format!("address book worker panicked: {join_err}").into()
                })
        }
        .boxed()
    }
}

/// Synchronous request handler. Runs on the blocking thread spawned by `call`.
fn handle_request(inner: &Arc<Mutex<AddressBook>>, req: AddressBookRequest) -> AddressBookResponse {
    use AddressBookRequest as Req;
    use AddressBookResponse as Resp;

    match req {
        Req::Get(addr) => {
            let mut book = lock(inner);
            Resp::MaybePeer(book.get(addr))
        }
        Req::Update(change) => {
            let mut book = lock(inner);
            Resp::MaybePeer(book.update(change))
        }
        Req::Extend(changes) => {
            let mut book = lock(inner);
            book.extend(changes);
            Resp::Unit
        }
        Req::AddPeer(addr) => {
            let mut book = lock(inner);
            Resp::Bool(book.add_peer(addr))
        }
        Req::Peers => {
            let book = lock(inner);
            Resp::Peers(book.peers().collect())
        }
        Req::StatePeers(state) => {
            let book = lock(inner);
            Resp::Peers(book.state_peers(state).collect())
        }
        Req::RecentlyLivePeers => {
            let now = Utc::now();
            let book = lock(inner);
            Resp::Peers(book.recently_live_peers(now))
        }
        Req::ReconnectionPeers => {
            let instant_now = Instant::now();
            let chrono_now = Utc::now();
            let book = lock(inner);
            Resp::Peers(book.reconnection_peers(instant_now, chrono_now).collect())
        }
        Req::MaybeConnectedPeers => {
            let instant_now = Instant::now();
            let chrono_now = Utc::now();
            let book = lock(inner);
            Resp::Peers(
                book.maybe_connected_peers(instant_now, chrono_now)
                    .collect(),
            )
        }
        Req::FreshGetAddrResponse => {
            let book = lock(inner);
            Resp::Peers(book.fresh_get_addr_response())
        }
        Req::TryFreshGetAddrResponse => match inner.try_lock() {
            Ok(book) => Resp::MaybePeers(Some(book.fresh_get_addr_response())),
            Err(TryLockError::WouldBlock) => Resp::MaybePeers(None),
            Err(TryLockError::Poisoned(_)) => panic_poisoned(),
        },
        Req::Cacheable => {
            let now = Utc::now();
            let book = lock(inner);
            Resp::Peers(book.cacheable(now))
        }
        Req::LocalListenerMetaAddr => {
            let book = lock(inner);
            Resp::MetaAddr(book.local_listener_meta_addr(Utc::now()))
        }
        Req::LocalListenerSocketAddr => {
            let book = lock(inner);
            Resp::SocketAddr(book.local_listener_socket_addr())
        }
        Req::NextReconnectionPeer => {
            let mut book = lock(inner);
            let instant_now = Instant::now();
            let chrono_now = Utc::now();

            let next = book.reconnection_peers(instant_now, chrono_now).next();
            let updated = next.and_then(|peer| {
                let reconnect = MetaAddr::new_reconnect(peer.addr);
                book.update(reconnect)
            });
            Resp::MaybePeer(updated)
        }
        Req::Bans => {
            let book = lock(inner);
            Resp::Bans(book.bans())
        }
        Req::Len => {
            let book = lock(inner);
            Resp::Count(book.len())
        }
        Req::PendingReconnectionAddr(addr) => {
            let mut book = lock(inner);
            Resp::Bool(book.pending_reconnection_addr(addr))
        }
    }
}

fn lock(inner: &Arc<Mutex<AddressBook>>) -> std::sync::MutexGuard<'_, AddressBook> {
    inner.lock().unwrap_or_else(|_| panic_poisoned())
}

#[cold]
fn panic_poisoned() -> ! {
    panic!("previous thread panicked while holding the address book mutex")
}

impl AddressBookPeers for AddressBookService {
    fn recently_live_peers(&self, now: chrono::DateTime<Utc>) -> Vec<MetaAddr> {
        lock(&self.inner).recently_live_peers(now)
    }

    fn add_peer(&mut self, peer: PeerSocketAddr) -> bool {
        lock(&self.inner).add_peer(peer)
    }
}
