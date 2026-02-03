//! Abstractions that represent "the rest of the network".
//!
//! # Implementation
//!
//! The [`PeerSet`] implementation is adapted from the one in [tower::Balance][tower-balance].
//!
//! As described in Tower's documentation, it:
//!
//! > Distributes requests across inner services using the [Power of Two Choices][p2c].
//! >
//! > As described in the [Finagle Guide][finagle]:
//! >
//! > > The algorithm randomly picks two services from the set of ready endpoints and
//! > > selects the least loaded of the two. By repeatedly using this strategy, we can
//! > > expect a manageable upper bound on the maximum load of any server.
//! > >
//! > > The maximum load variance between any two servers is bound by `ln(ln(n))` where
//! > > `n` is the number of servers in the cluster.
//!
//! The Power of Two Choices should work well for many network requests, but not all of them.
//! Some requests should only be made to a subset of connected peers.
//! For example, a request for a particular inventory item
//! should be made to a peer that has recently advertised that inventory hash.
//! Other requests require broadcasts, such as transaction diffusion.
//!
//! Implementing this specialized routing logic inside the `PeerSet` -- so that
//! it continues to abstract away "the rest of the network" into one endpoint --
//! is not a problem, as the `PeerSet` can simply maintain more information on
//! its peers and route requests appropriately. However, there is a problem with
//! maintaining accurate backpressure information, because the `Service` trait
//! requires that service readiness is independent of the data in the request.
//!
//! For this reason, in the future, this code will probably be refactored to
//! address this backpressure mismatch. One possibility is to refactor the code
//! so that one entity holds and maintains the peer set and metadata on the
//! peers, and each "backpressure category" of request is assigned to different
//! `Service` impls with specialized `poll_ready()` implementations. Another
//! less-elegant solution (which might be useful as an intermediate step for the
//! inventory case) is to provide a way to borrow a particular backing service,
//! say by address.
//!
//! [finagle]: https://twitter.github.io/finagle/guide/Clients.html#power-of-two-choices-p2c-least-loaded
//! [p2c]: http://www.eecs.harvard.edu/~michaelm/postscripts/handbook2001.pdf
//! [tower-balance]: https://github.com/tower-rs/tower/tree/master/tower/src/balance
//!
//! # Behavior During Network Upgrades
//!
//! [ZIP-201] specifies peer behavior during network upgrades:
//!
//! > With scheduled network upgrades, at the activation height, nodes on each consensus branch
//! > should disconnect from nodes on other consensus branches and only accept new incoming
//! > connections from nodes on the same consensus branch.
//!
//! Zebra handles this with the help of [`MinimumPeerVersion`], which determines the minimum peer
//! protocol version to accept based on the current best chain tip height. The minimum version is
//! therefore automatically increased when the block height reaches a network upgrade's activation
//! height. The helper type is then used to:
//!
//! - cancel handshakes to outdated peers, in `handshake::negotiate_version`
//! - cancel requests to and disconnect from peers that have become outdated, in
//!   [`PeerSet::push_unready`]
//! - disconnect from peers that have just responded and became outdated, in
//!   [`PeerSet::poll_unready`]
//! - disconnect from idle peers that have become outdated, in
//!   [`PeerSet::disconnect_from_outdated_peers`]
//!
//! ## Network Coalescence
//!
//! [ZIP-201] also specifies how Zcashd behaves [leading up to a activation
//! height][1]. Since Zcashd limits the number of connections to at most eight
//! peers, it will gradually migrate its connections to up-to-date peers as it
//! approaches the activation height.
//!
//! The motivation for this behavior is to avoid an abrupt partitioning the network, which can lead
//! to isolated peers and increases the chance of an eclipse attack on some peers of the network.
//!
//! Zebra does not gradually migrate its peers as it approaches an activation height. This is
//! because Zebra by default can connect to up to 75 peers, as can be seen in [`Config::default`].
//! Since this is a lot larger than the 8 peers Zcashd connects to, an eclipse attack becomes a lot
//! more costly to execute, and the probability of an abrupt network partition that isolates peers
//! is lower.
//!
//! Even if a Zebra node is manually configured to connect to a smaller number
//! of peers, the [`AddressBook`][2] is configured to hold a large number of
//! peer addresses ([`MAX_ADDRS_IN_ADDRESS_BOOK`][3]). Since the address book
//! prioritizes addresses it trusts (like those that it has successfully
//! connected to before), the node should be able to recover and rejoin the
//! network by itself, as long as the address book is populated with enough
//! entries.
//!
//! [1]: https://zips.z.cash/zip-0201#network-coalescence
//! [2]: crate::AddressBook
//! [3]: crate::constants::MAX_ADDRS_IN_ADDRESS_BOOK
//! [ZIP-201]: https://zips.z.cash/zip-0201

use std::{
    collections::{HashMap, HashSet},
    convert,
    fmt::Debug,
    marker::PhantomData,
    net::IpAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

use futures::{
    channel::{mpsc, oneshot},
    future::{FutureExt, TryFutureExt},
    prelude::*,
    stream::FuturesUnordered,
    task::noop_waker,
};
use indexmap::IndexMap;
use itertools::Itertools;
use num_integer::div_ceil;
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tower::{
    discover::{Change, Discover},
    load::Load,
    Service,
};

use zebra_chain::{chain_tip::ChainTip, parameters::Network};

use crate::{
    address_book::AddressMetrics,
    constants::MIN_PEER_SET_LOG_INTERVAL,
    peer::{LoadTrackedClient, MinimumPeerVersion},
    peer_set::{
        unready_service::{Error as UnreadyError, UnreadyService},
        InventoryChange, InventoryRegistry,
    },
    protocol::{
        external::InventoryHash,
        internal::{Request, Response},
    },
    BoxError, Config, PeerError, PeerSocketAddr, SharedPeerError,
};

#[cfg(test)]
mod tests;

/// A signal sent by the [`PeerSet`] when it has no ready peers, and gets a request from Zebra.
///
/// In response to this signal, the crawler tries to open more peer connections.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct MorePeers;

/// A signal sent by the [`PeerSet`] to cancel a [`Client`][1]'s current request
/// or response.
///
/// When it receives this signal, the [`Client`][1] stops processing and exits.
///
/// [1]: crate::peer::Client
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct CancelClientWork;

type ResponseFuture = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send + 'static>>;

/// A [`tower::Service`] that abstractly represents "the rest of the network".
///
/// # Security
///
/// The `Discover::Key` must be the transient remote address of each peer. This
/// address may only be valid for the duration of a single connection. (For
/// example, inbound connections have an ephemeral remote port, and proxy
/// connections have an ephemeral local or proxy port.)
///
/// Otherwise, malicious peers could interfere with other peers' `PeerSet` state.
pub struct PeerSet<D, C>
where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    // Peer Tracking: New Peers
    //
    /// Provides new and deleted peer [`Change`]s to the peer set,
    /// via the [`Discover`] trait implementation.
    discover: D,

    /// A channel that asks the peer crawler task to connect to more peers.
    demand_signal: mpsc::Sender<MorePeers>,

    /// A watch channel receiver with a copy of banned IP addresses.
    bans_receiver: watch::Receiver<Arc<IndexMap<IpAddr, std::time::Instant>>>,

    // Peer Tracking: Ready Peers
    //
    /// Connected peers that are ready to receive requests from Zebra,
    /// or send requests to Zebra.
    ready_services: HashMap<D::Key, D::Service>,

    // Request Routing
    //
    /// Stores gossiped inventory hashes from connected peers.
    ///
    /// Used to route inventory requests to peers that are likely to have it.
    inventory_registry: InventoryRegistry,

    /// Stores requests that should be routed to peers once they are ready.
    queued_broadcast_all: Option<(
        Request,
        tokio::sync::mpsc::Sender<ResponseFuture>,
        HashSet<D::Key>,
    )>,

    // Peer Tracking: Busy Peers
    //
    /// Connected peers that are handling a Zebra request,
    /// or Zebra is handling one of their requests.
    unready_services: FuturesUnordered<UnreadyService<D::Key, D::Service, Request>>,

    /// Channels used to cancel the request that an unready service is doing.
    cancel_handles: HashMap<D::Key, oneshot::Sender<CancelClientWork>>,

    // Peer Validation
    //
    /// An endpoint to see the minimum peer protocol version in real time.
    ///
    /// The minimum version depends on the block height, and [`MinimumPeerVersion`] listens for
    /// height changes and determines the correct minimum version.
    minimum_peer_version: MinimumPeerVersion<C>,

    /// The configured limit for inbound and outbound connections.
    ///
    /// The peer set panics if this size is exceeded.
    /// If that happens, our connection limit code has a bug.
    peerset_total_connection_limit: usize,

    // Background Tasks
    //
    /// Channel for passing ownership of tokio JoinHandles from PeerSet's background tasks
    ///
    /// The join handles passed into the PeerSet are used populate the `guards` member
    handle_rx: tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxError>>>>,

    /// Unordered set of handles to background tasks associated with the `PeerSet`
    ///
    /// These guards are checked for errors as part of `poll_ready` which lets
    /// the `PeerSet` propagate errors from background tasks back to the user
    guards: futures::stream::FuturesUnordered<JoinHandle<Result<(), BoxError>>>,

    // Metrics and Logging
    //
    /// Address book metrics watch channel.
    ///
    /// Used for logging diagnostics.
    address_metrics: watch::Receiver<AddressMetrics>,

    /// The last time we logged a message about the peer set size
    last_peer_log: Option<Instant>,

    /// The configured maximum number of peers that can be in the
    /// peer set per IP, defaults to [`crate::constants::DEFAULT_MAX_CONNS_PER_IP`]
    max_conns_per_ip: usize,

    /// The network of this peer set.
    network: Network,

    /// Set of addresses of established outbound peers.
    /// Used to accurately report the outbound connection count
    /// without counting pending handshakes.
    outbound_peers: HashSet<D::Key>,

    /// Set of addresses of established inbound peers.
    /// Used to accurately report the inbound connection count
    /// without counting pending handshakes.
    inbound_peers: HashSet<D::Key>,

    /// Established outbound connection count progress bar.
    #[cfg(feature = "progress-bar")]
    outbound_connection_bar: howudoin::Tx,

    /// Established inbound connection count progress bar.
    #[cfg(feature = "progress-bar")]
    inbound_connection_bar: howudoin::Tx,
}

impl<D, C> Drop for PeerSet<D, C>
where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    fn drop(&mut self) {
        #[cfg(feature = "progress-bar")]
        self.outbound_connection_bar.close();
        #[cfg(feature = "progress-bar")]
        self.inbound_connection_bar.close();

        // We don't have access to the current task (if any), so we just drop everything we can.
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        self.shut_down_tasks_and_channels(&mut cx);
    }
}

impl<D, C> PeerSet<D, C>
where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    #[allow(clippy::too_many_arguments)]
    /// Construct a peerset which uses `discover` to manage peer connections.
    ///
    /// Arguments:
    /// - `config`: configures the peer set connection limit;
    /// - `discover`: handles peer connects and disconnects;
    /// - `demand_signal`: requests more peers when all peers are busy (unready);
    /// - `handle_rx`: receives background task handles,
    ///   monitors them to make sure they're still running,
    ///   and shuts down all the tasks as soon as one task exits;
    /// - `inv_stream`: receives inventory changes from peers,
    ///   allowing the peer set to direct inventory requests;
    /// - `bans_receiver`: receives a map of banned IP addresses that should be dropped;
    /// - `address_book`: when peer set is busy, it logs address book diagnostics.
    /// - `minimum_peer_version`: endpoint to see the minimum peer protocol version in real time.
    /// - `max_conns_per_ip`: configured maximum number of peers that can be in the
    ///   peer set per IP, defaults to the config value or to
    ///   [`crate::constants::DEFAULT_MAX_CONNS_PER_IP`].
    pub fn new(
        config: &Config,
        discover: D,
        demand_signal: mpsc::Sender<MorePeers>,
        handle_rx: tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxError>>>>,
        inv_stream: broadcast::Receiver<InventoryChange>,
        bans_receiver: watch::Receiver<Arc<IndexMap<IpAddr, std::time::Instant>>>,
        address_metrics: watch::Receiver<AddressMetrics>,
        minimum_peer_version: MinimumPeerVersion<C>,
        max_conns_per_ip: Option<usize>,
    ) -> Self {
        Self {
            // New peers
            discover,
            demand_signal,
            // Banned peers
            bans_receiver,

            // Ready peers
            ready_services: HashMap::new(),
            // Request Routing
            inventory_registry: InventoryRegistry::new(inv_stream),
            queued_broadcast_all: None,

            // Busy peers
            unready_services: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),

            // Peer validation
            minimum_peer_version,
            peerset_total_connection_limit: config.peerset_total_connection_limit(),

            // Background tasks
            handle_rx,
            guards: futures::stream::FuturesUnordered::new(),

            // Metrics
            last_peer_log: None,
            address_metrics,

            max_conns_per_ip: max_conns_per_ip.unwrap_or(config.max_connections_per_ip),

            network: config.network.clone(),

            outbound_peers: HashSet::new(),
            inbound_peers: HashSet::new(),
            #[cfg(feature = "progress-bar")]
            outbound_connection_bar: howudoin::new_root().label("Outbound Connections"),
            #[cfg(feature = "progress-bar")]
            inbound_connection_bar: howudoin::new_root().label("Inbound Connections"),
        }
    }

    /// Check background task handles to make sure they're still running.
    ///
    /// Never returns `Ok`.
    ///
    /// If any background task exits, shuts down all other background tasks,
    /// and returns an error. Otherwise, returns `Pending`, and registers a wakeup for
    /// receiving the background tasks, or the background tasks exiting.
    fn poll_background_errors(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        futures::ready!(self.receive_tasks_if_needed(cx))?;

        // Return Pending if all background tasks are still running.
        match futures::ready!(Pin::new(&mut self.guards).poll_next(cx)) {
            Some(res) => {
                info!(
                    background_tasks = %self.guards.len(),
                    "a peer set background task exited, shutting down other peer set tasks"
                );

                self.shut_down_tasks_and_channels(cx);

                // Flatten the join result and inner result, and return any errors.
                res.map_err(Into::into)
                    // TODO: replace with Result::flatten when it stabilises (#70142)
                    .and_then(convert::identity)?;

                // Turn Ok() task exits into errors.
                Poll::Ready(Err("a peer set background task exited".into()))
            }

            None => {
                self.shut_down_tasks_and_channels(cx);
                Poll::Ready(Err("all peer set background tasks have exited".into()))
            }
        }
    }

    /// Receive background tasks, if they've been sent on the channel, but not consumed yet.
    ///
    /// Returns a result representing the current task state, or `Poll::Pending` if the background
    /// tasks should be polled again to check their state.
    fn receive_tasks_if_needed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        if self.guards.is_empty() {
            // Return Pending if the tasks have not been sent yet.
            let handles = futures::ready!(Pin::new(&mut self.handle_rx).poll(cx));

            match handles {
                // The tasks have been sent, but not consumed yet.
                Ok(handles) => {
                    // Currently, the peer set treats an empty background task set as an error.
                    //
                    // TODO: refactor `handle_rx` and `guards` into an enum
                    //       for the background task state: Waiting/Running/Shutdown.
                    assert!(
                        !handles.is_empty(),
                        "the peer set requires at least one background task"
                    );

                    self.guards.extend(handles);

                    Poll::Ready(Ok(()))
                }

                // The sender was dropped without sending the tasks.
                Err(_) => Poll::Ready(Err(
                    "sender did not send peer background tasks before it was dropped".into(),
                )),
            }
        } else {
            Poll::Ready(Ok(()))
        }
    }

    /// Shut down:
    /// - services by dropping the service lists
    /// - background tasks via their join handles or cancel handles
    /// - channels by closing the channel
    fn shut_down_tasks_and_channels(&mut self, cx: &mut Context<'_>) {
        // Drop services and cancel their background tasks.
        self.ready_services = HashMap::new();
        self.outbound_peers.clear();
        self.inbound_peers.clear();

        for (_peer_key, handle) in self.cancel_handles.drain() {
            let _ = handle.send(CancelClientWork);
        }
        self.unready_services = FuturesUnordered::new();

        // Close the MorePeers channel for all senders,
        // so we don't add more peers to a shut down peer set.
        self.demand_signal.close_channel();

        // Shut down background tasks, ignoring pending polls.
        self.handle_rx.close();
        let _ = self.receive_tasks_if_needed(cx);
        for guard in self.guards.iter() {
            guard.abort();
        }
    }

    /// Checks for newly ready, disconnects from outdated peers, and polls ready peer errors.
    fn poll_peers(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        // Check for newly ready peers, including newly added peers (which are added as unready).
        // So it needs to run after `poll_discover()`. Registers a wakeup if there are any unready
        // peers.
        //
        // Each connected peer should become ready within a few minutes, or timeout, close the
        // connection, and release its connection slot.
        //
        // TODO: drop peers that overload us with inbound messages and never become ready (#7822)
        let _poll_pending_or_ready: Poll<Option<()>> = self.poll_unready(cx)?;

        // Cleanup

        // Only checks the versions of ready peers, so it needs to run after `poll_unready()`.
        self.disconnect_from_outdated_peers();

        // Check for failures in ready peers, removing newly errored or disconnected peers.
        // So it needs to run after `poll_unready()`.
        self.poll_ready_peer_errors(cx).map(Ok)
    }

    /// Check busy peer services for request completion or errors.
    ///
    /// Move newly ready services to the ready list if they are for peers with supported protocol
    /// versions, otherwise they are dropped. Also drop failed services.
    ///
    /// Never returns an error.
    ///
    /// Returns `Ok(Some(())` if at least one peer became ready, `Poll::Pending` if there are
    /// unready peers, but none became ready, and `Ok(None)` if the unready peers were empty.
    ///
    /// If there are any remaining unready peers, registers a wakeup for the next time one becomes
    /// ready. If there are no unready peers, doesn't register any wakeups. (Since wakeups come
    /// from peers, there needs to be at least one peer to register a wakeup.)
    fn poll_unready(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<()>, BoxError>> {
        let mut result = Poll::Pending;

        // # Correctness
        //
        // `poll_next()` must always be called, because `self.unready_services` could have been
        // empty before the call to `self.poll_ready()`.
        //
        // > When new futures are added, `poll_next` must be called in order to begin receiving
        // > wake-ups for new futures.
        //
        // <https://docs.rs/futures/latest/futures/stream/futures_unordered/struct.FuturesUnordered.html>
        //
        // Returns Pending if we've finished processing the unready service changes,
        // but there are still some unready services.
        loop {
            // No ready peers left, but there are some unready peers pending.
            let Poll::Ready(ready_peer) = Pin::new(&mut self.unready_services).poll_next(cx) else {
                break;
            };

            match ready_peer {
                // No unready peers in the list.
                None => {
                    // If we've finished processing the unready service changes, and there are no
                    // unready services left, it doesn't make sense to return Pending, because
                    // their stream is terminated. But when we add more unready peers and call
                    // `poll_next()`, its termination status will be reset, and it will receive
                    // wakeups again.
                    if result.is_pending() {
                        result = Poll::Ready(Ok(None));
                    }

                    break;
                }

                // Unready -> Ready
                Some(Ok((key, svc))) => {
                    trace!(?key, "service became ready");

                    if self.bans_receiver.borrow().contains_key(&key.ip()) {
                        warn!(?key, "service is banned, dropping service");
                        self.outbound_peers.remove(&key);
                        self.inbound_peers.remove(&key);
                        std::mem::drop(svc);
                        continue;
                    }

                    self.push_ready(true, key, svc);

                    // Return Ok if at least one peer became ready.
                    result = Poll::Ready(Ok(Some(())));
                }

                // Unready -> Canceled
                Some(Err((key, UnreadyError::Canceled))) => {
                    // A service can be canceled because we've connected to the same peer twice.
                    //
                    // In that case, there can still be an active connection for this key
                    // (either unready with a cancel handle, or already moved to `ready_services`).
                    // Don't remove this key from `outbound_peers` unless there are no other
                    // tracked connections for it.
                    let duplicate_connection = self.has_peer_with_addr(key);
                    trace!(
                        ?key,
                        duplicate_connection,
                        "service was canceled, dropping service"
                    );

                    if !duplicate_connection {
                        self.outbound_peers.remove(&key);
                        self.inbound_peers.remove(&key);
                    }
                }
                Some(Err((key, UnreadyError::CancelHandleDropped(_)))) => {
                    // Similarly, services with dropped cancel handles can have duplicates.
                    // Avoid removing a peer entry if another connection is still tracked.
                    let duplicate_connection = self.has_peer_with_addr(key);
                    trace!(
                        ?key,
                        duplicate_connection,
                        "cancel handle was dropped, dropping service"
                    );

                    if !duplicate_connection {
                        self.outbound_peers.remove(&key);
                        self.inbound_peers.remove(&key);
                    }
                }

                // Unready -> Errored
                Some(Err((key, UnreadyError::Inner(error)))) => {
                    debug!(%error, "service failed while unready, dropping service");

                    self.outbound_peers.remove(&key);
                    self.inbound_peers.remove(&key);
                    let cancel = self.cancel_handles.remove(&key);
                    assert!(cancel.is_some(), "missing cancel handle");
                }
            }
        }

        result
    }

    /// Checks previously ready peer services for errors.
    ///
    /// The only way these peer `Client`s can become unready is when we send them a request,
    /// because the peer set has exclusive access to send requests to each peer. (If an inbound
    /// request is in progress, it will be handled, then our request will be sent by the connection
    /// task.)
    ///
    /// Returns `Poll::Ready` if there are some ready peers, and `Poll::Pending` if there are no
    /// ready peers. Registers a wakeup if any peer has failed due to a disconnection, hang, or protocol error.
    ///
    /// # Panics
    ///
    /// If any peers somehow became unready without being sent a request. This indicates a bug in the peer set, where requests
    /// are sent to peers without putting them in `unready_peers`.
    fn poll_ready_peer_errors(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let mut previous = HashMap::new();
        std::mem::swap(&mut previous, &mut self.ready_services);

        // TODO: consider only checking some peers each poll (for performance reasons),
        //       but make sure we eventually check all of them.
        for (key, mut svc) in previous.drain() {
            let Poll::Ready(peer_readiness) = Pin::new(&mut svc).poll_ready(cx) else {
                unreachable!(
                    "unexpected unready peer: peers must be put into the unready_peers list \
                     after sending them a request"
                );
            };

            match peer_readiness {
                // Still ready, add it back to the list.
                Ok(()) => {
                    if self.bans_receiver.borrow().contains_key(&key.ip()) {
                        debug!(?key, "service ip is banned, dropping service");
                        self.outbound_peers.remove(&key);
                        self.inbound_peers.remove(&key);
                        std::mem::drop(svc);
                        continue;
                    }

                    self.push_ready(false, key, svc)
                }

                // Ready -> Errored
                Err(error) => {
                    debug!(%error, "service failed while ready, dropping service");

                    self.outbound_peers.remove(&key);
                    self.inbound_peers.remove(&key);
                    // Ready services can just be dropped, they don't need any cleanup.
                    std::mem::drop(svc);
                }
            }
        }

        if self.ready_services.is_empty() {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    /// Returns the number of peer connections Zebra already has with
    /// the provided IP address
    ///
    /// # Performance
    ///
    /// This method is `O(connected peers)`, so it should not be called from a loop
    /// that is already iterating through the peer set.
    fn num_peers_with_ip(&self, ip: IpAddr) -> usize {
        self.ready_services
            .keys()
            .chain(self.cancel_handles.keys())
            .filter(|addr| addr.ip() == ip)
            .count()
    }

    /// Returns `true` if Zebra is already connected to the IP and port in `addr`.
    fn has_peer_with_addr(&self, addr: PeerSocketAddr) -> bool {
        self.ready_services.contains_key(&addr) || self.cancel_handles.contains_key(&addr)
    }

    /// Processes the entire list of newly inserted or removed services.
    ///
    /// Puts inserted services in the unready list.
    /// Drops removed services, after cancelling any pending requests.
    ///
    /// If the peer connector channel is closed, returns an error.
    ///
    /// Otherwise, returns `Ok` if it discovered at least one peer, or `Poll::Pending` if it didn't
    /// discover any peers. Always registers a wakeup for new peers, even when it returns `Ok`.
    fn poll_discover(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        // Return pending if there are no peers in the list.
        let mut result = Poll::Pending;

        loop {
            // If we've emptied the list, finish looping, otherwise process the new peer.
            let Poll::Ready(discovered) = Pin::new(&mut self.discover).poll_discover(cx) else {
                break;
            };

            // If the change channel has a permanent error, return that error.
            let change = discovered
                .ok_or("discovery stream closed")?
                .map_err(Into::into)?;

            // Otherwise we have successfully processed a peer.
            result = Poll::Ready(Ok(()));

            // Process each change.
            match change {
                Change::Remove(key) => {
                    trace!(?key, "got Change::Remove from Discover");
                    self.remove(&key);
                }
                Change::Insert(key, svc) => {
                    // We add peers as unready, so that we:
                    // - always do the same checks on every ready peer, and
                    // - check for any errors that happened right after the handshake
                    trace!(?key, "got Change::Insert from Discover");

                    // # Security
                    //
                    // Drop the new peer if we are already connected to it.
                    // Preferring old connections avoids connection thrashing.
                    if self.has_peer_with_addr(key) {
                        std::mem::drop(svc);
                        continue;
                    }

                    // # Security
                    //
                    // drop the new peer if there are already `max_conns_per_ip` peers with
                    // the same IP address in the peer set.
                    if self.num_peers_with_ip(key.ip()) >= self.max_conns_per_ip {
                        std::mem::drop(svc);
                        continue;
                    }

                    if svc.is_outbound() {
                        self.outbound_peers.insert(key);
                    } else if svc.is_inbound() {
                        self.inbound_peers.insert(key);
                    }

                    self.push_unready(key, svc);
                }
            }
        }

        result
    }

    /// Checks if the minimum peer version has changed, and disconnects from outdated peers.
    fn disconnect_from_outdated_peers(&mut self) {
        if let Some(minimum_version) = self.minimum_peer_version.changed() {
            let outdated: Vec<_> = self
                .ready_services
                .iter()
                .filter(|(_, peer)| peer.remote_version() < minimum_version)
                .map(|(addr, _)| *addr)
                .collect();

            for key in &outdated {
                self.outbound_peers.remove(key);
                self.inbound_peers.remove(key);
            }

            // It is ok to drop ready services, they don't need anything cancelled.
            self.ready_services
                .retain(|_address, peer| peer.remote_version() >= minimum_version);
        }
    }

    /// Takes a ready service by key.
    fn take_ready_service(&mut self, key: &D::Key) -> Option<D::Service> {
        if let Some(svc) = self.ready_services.remove(key) {
            assert!(
                !self.cancel_handles.contains_key(key),
                "cancel handles are only used for unready service work"
            );

            Some(svc)
        } else {
            None
        }
    }

    /// Remove the service corresponding to `key` from the peer set.
    ///
    /// Drops the service, cancelling any pending request or response to that peer.
    /// If the peer does not exist, does nothing.
    fn remove(&mut self, key: &D::Key) {
        self.outbound_peers.remove(key);
        self.inbound_peers.remove(key);

        if let Some(ready_service) = self.take_ready_service(key) {
            // A ready service has no work to cancel, so just drop it.
            std::mem::drop(ready_service);
        } else if let Some(handle) = self.cancel_handles.remove(key) {
            // Cancel the work, implicitly dropping the cancel handle.
            // The service future returns a `Canceled` error,
            // making `poll_unready` drop the service.
            let _ = handle.send(CancelClientWork);
        }
    }

    /// Adds a ready service to the ready list if it's for a peer with a supported version.
    /// If `was_unready` is true, also removes the peer's cancel handle.
    ///
    /// If the service is for a connection to an outdated peer, the service is dropped.
    fn push_ready(&mut self, was_unready: bool, key: D::Key, svc: D::Service) {
        let cancel = self.cancel_handles.remove(&key);
        assert_eq!(
            cancel.is_some(),
            was_unready,
            "missing or unexpected cancel handle"
        );

        if svc.remote_version() >= self.minimum_peer_version.current() {
            self.ready_services.insert(key, svc);
        } else {
            self.outbound_peers.remove(&key);
            self.inbound_peers.remove(&key);
            std::mem::drop(svc);
        }
    }

    /// Adds a busy service to the unready list if it's for a peer with a supported version,
    /// and adds a cancel handle for the service's current request.
    ///
    /// If the service is for a connection to an outdated peer, the request is cancelled and the
    /// service is dropped.
    fn push_unready(&mut self, key: D::Key, svc: D::Service) {
        let peer_version = svc.remote_version();
        let (tx, rx) = oneshot::channel();

        self.unready_services.push(UnreadyService {
            key: Some(key),
            service: Some(svc),
            cancel: rx,
            _req: PhantomData,
        });

        if peer_version >= self.minimum_peer_version.current() {
            self.cancel_handles.insert(key, tx);
        } else {
            // Cancel any request made to the service because it is using an outdated protocol
            // version.
            let _ = tx.send(CancelClientWork);
        }
    }

    /// Performs P2C on `self.ready_services` to randomly select a less-loaded ready service.
    fn select_ready_p2c_peer(&self) -> Option<D::Key> {
        self.select_p2c_peer_from_list(&self.ready_services.keys().copied().collect())
    }

    /// Performs P2C on `ready_service_list` to randomly select a less-loaded ready service.
    #[allow(clippy::unwrap_in_result)]
    fn select_p2c_peer_from_list(&self, ready_service_list: &HashSet<D::Key>) -> Option<D::Key> {
        match ready_service_list.len() {
            0 => None,
            1 => Some(
                *ready_service_list
                    .iter()
                    .next()
                    .expect("just checked there is one service"),
            ),
            len => {
                // Choose 2 random peers, then return the least loaded of those 2 peers.
                let (a, b) = {
                    let idxs = rand::seq::index::sample(&mut rand::thread_rng(), len, 2);
                    let a = idxs.index(0);
                    let b = idxs.index(1);

                    let a = *ready_service_list
                        .iter()
                        .nth(a)
                        .expect("sample returns valid indexes");
                    let b = *ready_service_list
                        .iter()
                        .nth(b)
                        .expect("sample returns valid indexes");

                    (a, b)
                };

                let a_load = self.query_load(&a).expect("supplied services are ready");
                let b_load = self.query_load(&b).expect("supplied services are ready");

                let selected = if a_load <= b_load { a } else { b };

                trace!(
                    a.key = ?a,
                    a.load = ?a_load,
                    b.key = ?b,
                    b.load = ?b_load,
                    selected = ?selected,
                    ?len,
                    "selected service by p2c"
                );

                Some(selected)
            }
        }
    }

    /// Randomly chooses `max_peers` ready services, ignoring service load.
    ///
    /// The chosen peers are unique, but their order is not fully random.
    fn select_random_ready_peers(&self, max_peers: usize) -> Vec<D::Key> {
        use rand::seq::IteratorRandom;

        self.ready_services
            .keys()
            .copied()
            .choose_multiple(&mut rand::thread_rng(), max_peers)
    }

    /// Accesses a ready endpoint by `key` and returns its current load.
    ///
    /// Returns `None` if the service is not in the ready service list.
    fn query_load(&self, key: &D::Key) -> Option<<D::Service as Load>::Metric> {
        let svc = self.ready_services.get(key);
        svc.map(|svc| svc.load())
    }

    /// Routes a request using P2C load-balancing.
    fn route_p2c(&mut self, req: Request) -> <Self as tower::Service<Request>>::Future {
        if let Some(p2c_key) = self.select_ready_p2c_peer() {
            tracing::trace!(?p2c_key, "routing based on p2c");

            let mut svc = self
                .take_ready_service(&p2c_key)
                .expect("selected peer must be ready");

            let fut = svc.call(req);
            self.push_unready(p2c_key, svc);

            return fut.map_err(Into::into).boxed();
        }

        async move {
            // Let other tasks run, so a retry request might get different ready peers.
            tokio::task::yield_now().await;

            // # Security
            //
            // Avoid routing requests to peers that are missing inventory.
            // If we kept trying doomed requests, peers that are missing our requested inventory
            // could take up a large amount of our bandwidth and retry limits.
            Err(SharedPeerError::from(PeerError::NoReadyPeers))
        }
        .map_err(Into::into)
        .boxed()
    }

    /// Tries to route a request to a ready peer that advertised that inventory,
    /// falling back to a ready peer that isn't missing the inventory.
    ///
    /// If all ready peers are missing the inventory,
    /// returns a synthetic [`NotFoundRegistry`](PeerError::NotFoundRegistry) error.
    ///
    /// Uses P2C to route requests to the least loaded peer in each list.
    fn route_inv(
        &mut self,
        req: Request,
        hash: InventoryHash,
    ) -> <Self as tower::Service<Request>>::Future {
        let advertising_peer_list = self
            .inventory_registry
            .advertising_peers(hash)
            .filter(|&addr| self.ready_services.contains_key(addr))
            .copied()
            .collect();

        // # Security
        //
        // Choose a random, less-loaded peer with the inventory.
        //
        // If we chose the first peer in HashMap order,
        // peers would be able to influence our choice by switching addresses.
        // But we need the choice to be random,
        // so that a peer can't provide all our inventory responses.
        let peer = self.select_p2c_peer_from_list(&advertising_peer_list);

        if let Some(mut svc) = peer.and_then(|key| self.take_ready_service(&key)) {
            let peer = peer.expect("just checked peer is Some");
            tracing::trace!(?hash, ?peer, "routing to a peer which advertised inventory");
            let fut = svc.call(req);
            self.push_unready(peer, svc);
            return fut.map_err(Into::into).boxed();
        }

        let missing_peer_list: HashSet<PeerSocketAddr> = self
            .inventory_registry
            .missing_peers(hash)
            .copied()
            .collect();
        let maybe_peer_list = self
            .ready_services
            .keys()
            .filter(|addr| !missing_peer_list.contains(addr))
            .copied()
            .collect();

        // Security: choose a random, less-loaded peer that might have the inventory.
        let peer = self.select_p2c_peer_from_list(&maybe_peer_list);

        if let Some(mut svc) = peer.and_then(|key| self.take_ready_service(&key)) {
            let peer = peer.expect("just checked peer is Some");
            tracing::trace!(?hash, ?peer, "routing to a peer that might have inventory");
            let fut = svc.call(req);
            self.push_unready(peer, svc);
            return fut.map_err(Into::into).boxed();
        }

        tracing::debug!(
            ?hash,
            "all ready peers are missing inventory, failing request"
        );

        async move {
            // Let other tasks run, so a retry request might get different ready peers.
            tokio::task::yield_now().await;

            // # Security
            //
            // Avoid routing requests to peers that are missing inventory.
            // If we kept trying doomed requests, peers that are missing our requested inventory
            // could take up a large amount of our bandwidth and retry limits.
            Err(SharedPeerError::from(PeerError::NotFoundRegistry(vec![
                hash,
            ])))
        }
        .map_err(Into::into)
        .boxed()
    }

    /// Routes the same request to up to `max_peers` ready peers, ignoring return values.
    ///
    /// `max_peers` must be at least one, and at most the number of ready peers.
    fn route_multiple(
        &mut self,
        req: Request,
        max_peers: usize,
    ) -> <Self as tower::Service<Request>>::Future {
        assert!(
            max_peers > 0,
            "requests must be routed to at least one peer"
        );
        assert!(
            max_peers <= self.ready_services.len(),
            "requests can only be routed to ready peers"
        );

        let selected_peers = self.select_random_ready_peers(max_peers);
        self.send_multiple(req, selected_peers)
    }

    /// Sends the same request to the provided ready peers, ignoring return values.
    ///
    /// # Security
    ///
    /// Callers should choose peers randomly, ignoring load.
    /// This avoids favouring malicious peers, because peers can influence their own load.
    ///
    /// The order of peers isn't completely random,
    /// but peer request order is not security-sensitive.
    fn send_multiple(
        &mut self,
        req: Request,
        peers: Vec<D::Key>,
    ) -> <Self as tower::Service<Request>>::Future {
        let futs = FuturesUnordered::new();
        for key in peers {
            let mut svc = self
                .take_ready_service(&key)
                .expect("selected peers are ready");
            futs.push(svc.call(req.clone()).map_err(|_| ()));
            self.push_unready(key, svc);
        }

        async move {
            let results = futs.collect::<Vec<Result<_, _>>>().await;
            tracing::debug!(
                ok.len = results.iter().filter(|r| r.is_ok()).count(),
                err.len = results.iter().filter(|r| r.is_err()).count(),
                "sent peer request to multiple peers"
            );
            Ok(Response::Nil)
        }
        .boxed()
    }

    /// Broadcasts the same request to lots of ready peers, ignoring return values.
    fn route_broadcast(&mut self, req: Request) -> <Self as tower::Service<Request>>::Future {
        // Broadcasts ignore the response
        self.route_multiple(req, self.number_of_peers_to_broadcast())
    }

    /// Broadcasts the same request to all ready peers, ignoring return values.
    fn broadcast_all(&mut self, req: Request) -> <Self as tower::Service<Request>>::Future {
        let ready_peers = self.ready_services.keys().copied().collect();
        let send_multiple_fut = self.send_multiple(req.clone(), ready_peers);
        let Some(mut queued_broadcast_fut_receiver) = self.queue_broadcast_all_unready(&req) else {
            return send_multiple_fut;
        };

        async move {
            let _ = send_multiple_fut.await?;
            while queued_broadcast_fut_receiver.recv().await.is_some() {}
            Ok(Response::Nil)
        }
        .boxed()
    }

    /// If there are unready peers, queues a request to be broadcasted to them and
    /// returns a channel receiver for callers to await the broadcast_all() futures, or
    /// returns None if there are no unready peers.
    fn queue_broadcast_all_unready(
        &mut self,
        req: &Request,
    ) -> Option<tokio::sync::mpsc::Receiver<ResponseFuture>> {
        if !self.cancel_handles.is_empty() {
            /// How many broadcast all futures to send to the channel until the peer set should wait for the channel consumer
            /// to read a message before continuing to send the queued broadcast request to peers that were originally unready.
            const QUEUED_BROADCAST_FUTS_CHANNEL_SIZE: usize = 3;

            let (sender, receiver) = tokio::sync::mpsc::channel(QUEUED_BROADCAST_FUTS_CHANNEL_SIZE);
            let unready_peers: HashSet<_> = self.cancel_handles.keys().cloned().collect();
            let queued = (req.clone(), sender, unready_peers);

            // Drop the existing queued broadcast all request, if any.
            self.queued_broadcast_all = Some(queued);

            Some(receiver)
        } else {
            None
        }
    }

    /// Broadcasts the same requests to all ready peers which were unready when
    /// [`PeerSet::broadcast_all()`] was last called, ignoring return values.
    fn broadcast_all_queued(&mut self) {
        let Some((req, sender, mut remaining_peers)) = self.queued_broadcast_all.take() else {
            return;
        };

        let Ok(reserved_send_slot) = sender.try_reserve() else {
            self.queued_broadcast_all = Some((req, sender, remaining_peers));
            return;
        };

        let peers: Vec<_> = self
            .ready_services
            .keys()
            .filter(|ready_peer| remaining_peers.remove(ready_peer))
            .copied()
            .collect();

        reserved_send_slot.send(self.send_multiple(req.clone(), peers).boxed());

        if !remaining_peers.is_empty() {
            self.queued_broadcast_all = Some((req, sender, remaining_peers));
        }
    }

    /// Given a number of ready peers calculate to how many of them Zebra will
    /// actually send the request to. Return this number.
    pub(crate) fn number_of_peers_to_broadcast(&self) -> usize {
        if self.network.is_regtest() {
            // In regtest, we broadcast to all peers, so that we can test the
            // peer set with a small number of peers.
            self.ready_services.len()
        } else {
            // We are currently sending broadcast messages to a third of the total peers.
            const PEER_FRACTION_TO_BROADCAST: usize = 3;

            // Round up, so that if we have one ready peer, it gets the request.
            div_ceil(self.ready_services.len(), PEER_FRACTION_TO_BROADCAST)
        }
    }

    /// Returns the list of addresses in the peer set.
    fn peer_set_addresses(&self) -> Vec<PeerSocketAddr> {
        self.ready_services
            .keys()
            .chain(self.cancel_handles.keys())
            .cloned()
            .collect()
    }

    /// Logs the peer set size, and any potential connectivity issues.
    fn log_peer_set_size(&mut self) {
        let ready_services_len = self.ready_services.len();
        let unready_services_len = self.unready_services.len();
        trace!(ready_peers = ?ready_services_len, unready_peers = ?unready_services_len);

        let now = Instant::now();

        // These logs are designed to be human-readable in a terminal, at the
        // default Zebra log level. If you need to know the peer set size for
        // every request, use the trace-level logs, or the metrics exporter.
        if let Some(last_peer_log) = self.last_peer_log {
            // Avoid duplicate peer set logs
            if now.duration_since(last_peer_log) < MIN_PEER_SET_LOG_INTERVAL {
                return;
            }
        } else {
            // Suppress initial logs until the peer set has started up.
            // There can be multiple initial requests before the first peer is
            // ready.
            self.last_peer_log = Some(now);
            return;
        }

        self.last_peer_log = Some(now);

        // Log potential duplicate connections.
        let peers = self.peer_set_addresses();

        // Check for duplicates by address and port: these are unexpected and represent a bug.
        let duplicates: Vec<PeerSocketAddr> = peers.iter().duplicates().cloned().collect();

        let mut peer_counts = peers.iter().counts();
        peer_counts.retain(|peer, _count| duplicates.contains(peer));

        if !peer_counts.is_empty() {
            let duplicate_connections: usize = peer_counts.values().sum();

            warn!(
                ?duplicate_connections,
                duplicated_peers = ?peer_counts.len(),
                peers = ?peers.len(),
                "duplicate peer connections in peer set"
            );
        }

        // Check for duplicates by address: these can happen if there are multiple nodes
        // behind a NAT or on a single server.
        let peers: Vec<IpAddr> = peers.iter().map(|addr| addr.ip()).collect();
        let duplicates: Vec<IpAddr> = peers.iter().duplicates().cloned().collect();

        let mut peer_counts = peers.iter().counts();
        peer_counts.retain(|peer, _count| duplicates.contains(peer));

        if !peer_counts.is_empty() {
            let duplicate_connections: usize = peer_counts.values().sum();

            info!(
                ?duplicate_connections,
                duplicated_peers = ?peer_counts.len(),
                peers = ?peers.len(),
                "duplicate IP addresses in peer set"
            );
        }

        // Only log connectivity warnings if all our peers are busy (or there are no peers).
        if ready_services_len > 0 {
            return;
        }

        let address_metrics = *self.address_metrics.borrow();
        if unready_services_len == 0 {
            warn!(
                ?address_metrics,
                "network request with no peer connections. Hint: check your network connection"
            );
        } else {
            info!(?address_metrics, "network request with no ready peers: finding more peers, waiting for {} peers to answer requests",
                  unready_services_len);
        }
    }

    /// Updates the peer set metrics.
    ///
    /// # Panics
    ///
    /// If the peer set size exceeds the connection limit.
    fn update_metrics(&self) {
        let num_ready = self.ready_services.len();
        let num_unready = self.unready_services.len();
        let num_peers = num_ready + num_unready;
        metrics::gauge!("pool.num_ready").set(num_ready as f64);
        metrics::gauge!("pool.num_unready").set(num_unready as f64);
        metrics::gauge!("zcash.net.peers").set(num_peers as f64);

        let num_outbound = self.outbound_peers.len();
        let num_inbound = self.inbound_peers.len();
        metrics::gauge!("zcash.net.peers.outbound").set(num_outbound as f64);
        metrics::gauge!("zcash.net.peers.inbound").set(num_inbound as f64);

        #[cfg(feature = "progress-bar")]
        self.outbound_connection_bar
            .set_pos(u64::try_from(num_outbound).expect("fits in u64"));
        #[cfg(feature = "progress-bar")]
        self.inbound_connection_bar
            .set_pos(u64::try_from(num_inbound).expect("fits in u64"));

        // Security: make sure we haven't exceeded the connection limit
        if num_peers > self.peerset_total_connection_limit {
            let address_metrics = *self.address_metrics.borrow();
            panic!(
                "unexpectedly exceeded configured peer set connection limit: \n\
                 peers: {num_peers:?}, ready: {num_ready:?}, unready: {num_unready:?}, \n\
                 address_metrics: {address_metrics:?}",
            );
        }
    }
}

impl<D, C> Service<Request> for PeerSet<D, C>
where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Update service and peer statuses.
        //
        // # Correctness
        //
        // All of the futures that receive a context from this method can wake the peer set buffer
        // task. If there are no ready peers, and no new peers, network requests will pause until:
        // - an unready peer becomes ready, or
        // - a new peer arrives.

        // Check for new peers, and register a task wakeup when the next new peers arrive. New peers
        // can be infrequent if our connection slots are full, or we're connected to all
        // available/useful peers.
        let _poll_pending_or_ready: Poll<()> = self.poll_discover(cx)?;

        // These tasks don't provide new peers or newly ready peers.
        let _poll_pending: Poll<()> = self.poll_background_errors(cx)?;
        let _poll_pending_or_ready: Poll<()> = self.inventory_registry.poll_inventory(cx)?;

        let ready_peers = self.poll_peers(cx)?;

        // These metrics should run last, to report the most up-to-date information.
        self.log_peer_set_size();
        self.update_metrics();

        if ready_peers.is_pending() {
            // # Correctness
            //
            // If the channel is full, drop the demand signal rather than waiting. If we waited
            // here, the crawler could deadlock sending a request to fetch more peers, because it
            // also empties the channel.
            trace!("no ready services, sending demand signal");
            let _ = self.demand_signal.try_send(MorePeers);

            // # Correctness
            //
            // The current task must be scheduled for wakeup every time we return `Poll::Pending`.
            //
            // As long as there are unready or new peers, this task will run, because:
            // - `poll_discover` schedules this task for wakeup when new peers arrive.
            // - if there are unready peers, `poll_unready` or `poll_ready_peers` schedule this
            //   task for wakeup when peer services become ready.
            //
            // To avoid peers blocking on a full peer status/error channel:
            // - `poll_background_errors` schedules this task for wakeup when the peer status
            //   update task exits.
            return Poll::Pending;
        }

        self.broadcast_all_queued();

        if self.ready_services.is_empty() {
            self.poll_peers(cx)
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let fut = match req {
            // Only do inventory-aware routing on individual items.
            Request::BlocksByHash(ref hashes) if hashes.len() == 1 => {
                let hash = InventoryHash::from(*hashes.iter().next().unwrap());
                self.route_inv(req, hash)
            }
            Request::TransactionsById(ref hashes) if hashes.len() == 1 => {
                let hash = InventoryHash::from(*hashes.iter().next().unwrap());
                self.route_inv(req, hash)
            }

            // Broadcast advertisements to lots of peers
            Request::AdvertiseTransactionIds(_) => self.route_broadcast(req),
            Request::AdvertiseBlock(_) => self.route_broadcast(req),
            Request::AdvertiseBlockToAll(_) => self.broadcast_all(req),

            // Choose a random less-loaded peer for all other requests
            _ => self.route_p2c(req),
        };
        self.update_metrics();

        fut
    }
}
