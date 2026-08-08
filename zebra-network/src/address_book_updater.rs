//! The address book updater: an actor-style task that owns the address book
//! write path, wrapped in a [`tower::buffer::Buffer`] service handle.

use std::{
    cmp::max,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

use chrono::Utc;
use futures::future;
use indexmap::IndexMap;
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tower::{buffer::Buffer, util::BoxService, Service};
use tracing::{Instrument, Level, Span};

use crate::{
    address_book::AddressMetrics,
    address_book_peers::AddressBookPeers,
    meta_addr::{MetaAddr, MetaAddrChange},
    AddressBook, BoxError, Config,
};

#[cfg(test)]
mod tests;

/// The minimum size of the address book updater channel.
pub const MIN_CHANNEL_SIZE: usize = 10;

/// The `AddressBookUpdater` hooks into incoming message streams for each peer
/// and lets the owner of the sender handle update the address book. For
/// example, it can be used to record per-connection last-seen timestamps, or
/// add new initial peers to the address book.
///
/// It also serves [`AddressBookRequest`]s through a [`Buffer`]-wrapped
/// [`AddressBookService`], which shares a single ordered request queue with
/// the change events. This makes compound operations like
/// [`AddressBookRequest::NextReconnectPeer`] atomic by construction.
#[derive(Debug, Eq, PartialEq)]
pub struct AddressBookUpdater;

#[derive(Copy, Clone, Debug, Error, Eq, PartialEq, Hash)]
#[error("all address book updater senders are closed")]
pub struct AllAddressBookUpdaterSendersClosed;

/// A request to the address book updater task.
#[derive(Clone, Debug)]
pub enum AddressBookRequest {
    /// Apply a single [`MetaAddrChange`] to the address book.
    Change(MetaAddrChange),

    /// Extend the address book with a batch of validated gossiped changes.
    ///
    /// The address book handles duplicate addresses internally.
    ExtendGossiped(Vec<MetaAddrChange>),

    /// Atomically choose the next reconnection candidate, and mark it as
    /// [`AttemptPending`](crate::PeerAddrState::AttemptPending).
    ///
    /// Because the address book updater serves one request at a time,
    /// concurrent `NextReconnectPeer` requests never return the same peer.
    NextReconnectPeer,

    /// Return the peers that are considered alive, in connection order.
    //
    // Hot reads like `getpeerinfo` currently use the shared address book
    // handle instead of this request, so they stay off the write queue.
    #[allow(dead_code)]
    RecentlyLivePeers,

    /// Return the peers that should be written to the peer cache on disk.
    CacheablePeers,

    /// Return the number of candidate peers that are currently ready for a
    /// connection attempt.
    ///
    /// The returned count is a snapshot: candidates can become ready or be
    /// attempted by other tasks immediately afterwards.
    ReadyPeerCount,
}

/// A response from the address book updater task.
#[derive(Clone, Debug)]
pub enum AddressBookResponse {
    /// The updated address book entry,
    /// or `None` if the change was rejected or delayed.
    //
    // Production changes are fire-and-forget, so the updated entry is
    // currently only read by tests.
    Updated(#[allow(dead_code)] Option<MetaAddr>),

    /// The address book was extended with the gossiped changes.
    Extended,

    /// The next reconnection candidate, already marked as
    /// [`AttemptPending`](crate::PeerAddrState::AttemptPending),
    /// or `None` if no peers are ready for a connection attempt.
    NextReconnectPeer(Option<MetaAddr>),

    /// A list of peers, in response to
    /// [`RecentlyLivePeers`](AddressBookRequest::RecentlyLivePeers) or
    /// [`CacheablePeers`](AddressBookRequest::CacheablePeers).
    Peers(Vec<MetaAddr>),

    /// The number of candidate peers that are currently ready for a
    /// connection attempt.
    ReadyPeerCount(usize),
}

/// A queued call to the address book updater task.
///
/// Fire-and-forget change events don't have a response sender.
#[derive(Debug)]
pub struct AddressBookCall {
    /// The request to serve.
    request: AddressBookRequest,

    /// The channel used to send the response, if the caller wants one.
    rsp_tx: Option<oneshot::Sender<AddressBookResponse>>,
}

/// A cheap cloneable handle that sends fire-and-forget [`MetaAddrChange`]
/// events to the address book updater task.
///
/// Changes share the updater's single ordered queue with
/// [`AddressBookService`] requests.
#[derive(Clone, Debug)]
pub struct AddressBookChangeSender(mpsc::Sender<AddressBookCall>);

impl AddressBookChangeSender {
    /// Sends `change` to the address book updater task,
    /// waiting until the queue has spare capacity.
    ///
    /// Returns an error if the address book updater task has exited.
    pub async fn send(
        &self,
        change: MetaAddrChange,
    ) -> Result<(), AllAddressBookUpdaterSendersClosed> {
        self.0
            .send(AddressBookCall {
                request: AddressBookRequest::Change(change),
                rsp_tx: None,
            })
            .await
            .map_err(|_| AllAddressBookUpdaterSendersClosed)
    }
}

/// Creates an [`AddressBookChangeSender`] and the receiving half of its
/// channel, without spawning an updater task.
///
/// Used to create stub change senders in tests, and for isolated connections
/// that don't have an address book.
pub fn change_channel(size: usize) -> (AddressBookChangeSender, mpsc::Receiver<AddressBookCall>) {
    let (tx, rx) = mpsc::channel(size);
    (AddressBookChangeSender(tx), rx)
}

/// Serves [`AddressBookRequest`]s directly from the address book it holds.
///
/// Used both as the inner service behind [`AddressBookService`], and by the
/// updater task that drains fire-and-forget [`AddressBookChangeSender`] events.
#[derive(Clone)]
struct AddressBookHandler {
    /// The address book to read and update.
    address_book: Arc<std::sync::Mutex<AddressBook>>,

    /// The channel used to publish the ban list when it changes.
    bans_sender: Arc<watch::Sender<Arc<IndexMap<IpAddr, Instant>>>>,
}

impl AddressBookHandler {
    /// Serves a single `request`.
    ///
    /// # Correctness
    ///
    /// Briefly holds the address book threaded mutex, with no awaits while it is
    /// held. External tasks must only use that mutex for hot reads, so this never
    /// blocks for long.
    fn handle(&self, request: AddressBookRequest) -> AddressBookResponse {
        trace!(?request, "got address book request");

        match request {
            AddressBookRequest::Change(event) => {
                let event_ip = event.addr().ip();
                let updated = self
                    .address_book
                    .lock()
                    .expect("mutex should be unpoisoned")
                    .update(event);

                // `UpdateMisbehavior` events should only be passed to `update()` here,
                // so that this channel is always updated when new addresses are banned.
                if updated.is_none() {
                    let bans = self
                        .address_book
                        .lock()
                        .expect("mutex should be unpoisoned")
                        .bans();

                    if bans.contains_key(&event_ip) {
                        let _ = self.bans_sender.send(bans);
                    }
                }

                AddressBookResponse::Updated(updated)
            }

            AddressBookRequest::ExtendGossiped(changes) => {
                // Extend handles duplicate addresses internally.
                self.address_book
                    .lock()
                    .expect("mutex should be unpoisoned")
                    .extend(changes);

                AddressBookResponse::Extended
            }

            AddressBookRequest::NextReconnectPeer => {
                let mut guard = self
                    .address_book
                    .lock()
                    .expect("mutex should be unpoisoned");

                // Now we have the lock, get the current time
                let instant_now = Instant::now();
                let chrono_now = Utc::now();

                // Choose the next candidate, and mark it as `AttemptPending`,
                // in a single atomic request.
                let next_peer = guard
                    .reconnection_peers(instant_now, chrono_now)
                    .next()
                    .map(|next_peer| MetaAddr::new_reconnect(next_peer.addr));
                let next_peer = next_peer.and_then(|change| guard.update(change));

                AddressBookResponse::NextReconnectPeer(next_peer)
            }

            AddressBookRequest::RecentlyLivePeers => {
                let chrono_now = Utc::now();

                AddressBookResponse::Peers(
                    self.address_book
                        .lock()
                        .expect("mutex should be unpoisoned")
                        .recently_live_peers(chrono_now),
                )
            }

            AddressBookRequest::CacheablePeers => {
                let chrono_now = Utc::now();

                AddressBookResponse::Peers(
                    self.address_book
                        .lock()
                        .expect("mutex should be unpoisoned")
                        .cacheable(chrono_now),
                )
            }

            AddressBookRequest::ReadyPeerCount => {
                // Now we have the lock, get the current time
                let instant_now = Instant::now();
                let chrono_now = Utc::now();

                let ready_peer_count = self
                    .address_book
                    .lock()
                    .expect("mutex should be unpoisoned")
                    .reconnection_peers(instant_now, chrono_now)
                    .count();

                AddressBookResponse::ReadyPeerCount(ready_peer_count)
            }
        }
    }
}

impl Service<AddressBookRequest> for AddressBookHandler {
    type Response = AddressBookResponse;
    type Error = BoxError;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: AddressBookRequest) -> Self::Future {
        future::ready(Ok(self.handle(request)))
    }
}

/// A shared [`Buffer`]-wrapped service handle to the address book.
///
/// Requests are served one at a time, directly from the address book. Writes are
/// serialised with fire-and-forget [`AddressBookChangeSender`] events by the
/// address book mutex.
pub type AddressBookService =
    Buffer<BoxService<AddressBookRequest, AddressBookResponse, BoxError>, AddressBookRequest>;

impl AddressBookUpdater {
    /// Spawn a new [`AddressBookUpdater`] task, updating a new [`AddressBook`]
    /// configured with Zebra's actual `local_listener` address.
    ///
    /// Returns handles for:
    /// - the address book, which should only be used for hot reads,
    /// - a watch channel for the ban list,
    /// - the transmission channel for address book update events,
    /// - a buffered service for address book requests,
    /// - a watch channel for address book metrics, and
    /// - the address book updater task join handle.
    ///
    /// The task exits with an error when all the returned
    /// [`AddressBookChangeSender`]s and [`AddressBookService`]s are closed.
    pub fn spawn(
        config: &Config,
        local_listener: SocketAddr,
    ) -> (
        Arc<std::sync::Mutex<AddressBook>>,
        watch::Receiver<Arc<IndexMap<IpAddr, Instant>>>,
        AddressBookChangeSender,
        AddressBookService,
        watch::Receiver<AddressMetrics>,
        JoinHandle<Result<(), BoxError>>,
    ) {
        let address_book = AddressBook::new(
            local_listener,
            &config.network,
            config.max_connections_per_ip,
            span!(Level::TRACE, "address book"),
        );

        // Use an update channel and buffer based on the maximum number of
        // inbound and outbound peers.
        let channel_size = max(config.peerset_total_connection_limit(), MIN_CHANNEL_SIZE);

        Self::spawn_with_address_book(address_book, channel_size)
    }

    /// Spawn a new [`AddressBookUpdater`] task for an existing `address_book`,
    /// using `channel_size` for the update channel and service buffer.
    ///
    /// See [`AddressBookUpdater::spawn`] for details.
    pub fn spawn_with_address_book(
        address_book: AddressBook,
        channel_size: usize,
    ) -> (
        Arc<std::sync::Mutex<AddressBook>>,
        watch::Receiver<Arc<IndexMap<IpAddr, Instant>>>,
        AddressBookChangeSender,
        AddressBookService,
        watch::Receiver<AddressMetrics>,
        JoinHandle<Result<(), BoxError>>,
    ) {
        // Create an mpsc channel for both fire-and-forget address book update
        // events and buffered service requests, so all address book writes go
        // through a single ordered queue.
        let (worker_tx, mut worker_rx) = mpsc::channel::<AddressBookCall>(channel_size);

        let address_metrics = address_book.address_metrics_watcher();
        let address_book = Arc::new(std::sync::Mutex::new(address_book));

        #[cfg(feature = "progress-bar")]
        let (mut address_info, address_bar, never_bar, failed_bar) = {
            let address_bar = howudoin::new_root().label("Known Peers");
            let never_bar =
                howudoin::new_with_parent(address_bar.id()).label("Never Attempted Peers");
            let failed_bar = howudoin::new_with_parent(never_bar.id()).label("Failed Peers");

            (address_metrics.clone(), address_bar, never_bar, failed_bar)
        };

        let (bans_sender, bans_receiver) = tokio::sync::watch::channel(
            address_book
                .lock()
                .expect("mutex should be unpoisoned")
                .bans(),
        );

        let handler = AddressBookHandler {
            address_book: address_book.clone(),
            bans_sender: Arc::new(bans_sender),
        };
        let worker_handler = handler.clone();

        let worker = async move {
            info!("starting the address book updater");

            while let Some(AddressBookCall { request, rsp_tx }) = worker_rx.recv().await {
                let response = worker_handler.handle(request);

                if let Some(rsp_tx) = rsp_tx {
                    // The caller might have been cancelled, so ignore send errors.
                    let _ = rsp_tx.send(response);
                }

                #[cfg(feature = "progress-bar")]
                if matches!(howudoin::cancelled(), Some(true)) {
                    address_bar.close();
                    never_bar.close();
                    failed_bar.close();
                } else if address_info.has_changed()? {
                    // We don't track:
                    // - attempt pending because it's always small
                    // - responded because it's the remaining attempted-but-not-failed peers
                    // - recently live because it's similar to the connected peer counts

                    let address_info = *address_info.borrow_and_update();

                    address_bar
                        .set_pos(u64::try_from(address_info.num_addresses).expect("fits in u64"));
                    // .set_len(u64::try_from(address_info.address_limit).expect("fits in u64"));

                    never_bar.set_pos(
                        u64::try_from(address_info.never_attempted_gossiped).expect("fits in u64"),
                    );
                    // .set_len(u64::try_from(address_info.address_limit).expect("fits in u64"));

                    failed_bar.set_pos(u64::try_from(address_info.failed).expect("fits in u64"));
                    // .set_len(u64::try_from(address_info.address_limit).expect("fits in u64"));
                }
            }

            #[cfg(feature = "progress-bar")]
            {
                address_bar.close();
                never_bar.close();
                failed_bar.close();
            }

            let error = Err(AllAddressBookUpdaterSendersClosed.into());
            info!(?error, "stopping address book updater");
            error
        };

        // # Correctness
        //
        // The updater task is an async task, not a blocking thread:
        // - each request only briefly locks the address book mutex, with no
        //   awaits while it is held, so the task never blocks for long, and
        // - a long-lived blocking task would inhibit auto-advance in tests
        //   that pause the tokio clock.
        let span = Span::current();
        let address_book_updater_task_handle = tokio::spawn(worker.instrument(span));

        let change_sender = AddressBookChangeSender(worker_tx.clone());

        let address_book_service = Buffer::new(BoxService::new(handler), channel_size);

        (
            address_book,
            bans_receiver,
            change_sender,
            address_book_service,
            address_metrics,
            address_book_updater_task_handle,
        )
    }
}
