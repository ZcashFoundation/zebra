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
    future::Future,
    marker::PhantomData,
    net::IpAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use futures::{
    channel::{mpsc, oneshot},
    future::{FutureExt, TryFutureExt},
    prelude::*,
    stream::FuturesUnordered,
};
use itertools::Itertools;
use num_integer::div_ceil;
use tokio::{
    sync::{broadcast, oneshot::error::TryRecvError, watch},
    task::JoinHandle,
};
use tower::{
    discover::{Change, Discover},
    load::Load,
    Service,
};

use zebra_chain::chain_tip::ChainTip;

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

    // Peer Tracking: Ready Peers
    //
    /// Connected peers that are ready to receive requests from Zebra,
    /// or send requests to Zebra.
    ready_services: HashMap<D::Key, D::Service>,

    // Request Routing
    //
    /// A preselected ready service.
    ///
    /// # Correctness
    ///
    /// If this is `Some(addr)`, `addr` must be a key for a peer in `ready_services`.
    /// If that peer is removed from `ready_services`, we must set the preselected peer to `None`.
    ///
    /// This is handled by [`PeerSet::take_ready_service`] and
    /// [`PeerSet::disconnect_from_outdated_peers`].
    preselected_p2c_peer: Option<D::Key>,

    /// Stores gossiped inventory hashes from connected peers.
    ///
    /// Used to route inventory requests to peers that are likely to have it.
    inventory_registry: InventoryRegistry,

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
}

impl<D, C> Drop for PeerSet<D, C>
where
    D: Discover<Key = PeerSocketAddr, Service = LoadTrackedClient> + Unpin,
    D::Error: Into<BoxError>,
    C: ChainTip,
{
    fn drop(&mut self) {
        self.shut_down_tasks_and_channels()
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
    ///                monitors them to make sure they're still running,
    ///                and shuts down all the tasks as soon as one task exits;
    /// - `inv_stream`: receives inventory changes from peers,
    ///                 allowing the peer set to direct inventory requests;
    /// - `address_book`: when peer set is busy, it logs address book diagnostics.
    /// - `minimum_peer_version`: endpoint to see the minimum peer protocol version in real time.
    /// - `max_conns_per_ip`: configured maximum number of peers that can be in the
    ///                       peer set per IP, defaults to the config value or to
    ///                       [`crate::constants::DEFAULT_MAX_CONNS_PER_IP`].
    pub fn new(
        config: &Config,
        discover: D,
        demand_signal: mpsc::Sender<MorePeers>,
        handle_rx: tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxError>>>>,
        inv_stream: broadcast::Receiver<InventoryChange>,
        address_metrics: watch::Receiver<AddressMetrics>,
        minimum_peer_version: MinimumPeerVersion<C>,
        max_conns_per_ip: Option<usize>,
    ) -> Self {
        Self {
            // New peers
            discover,
            demand_signal,

            // Ready peers
            ready_services: HashMap::new(),
            // Request Routing
            preselected_p2c_peer: None,
            inventory_registry: InventoryRegistry::new(inv_stream),

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
        }
    }

    /// Check background task handles to make sure they're still running.
    ///
    /// If any background task exits, shuts down all other background tasks,
    /// and returns an error.
    fn poll_background_errors(&mut self, cx: &mut Context) -> Result<(), BoxError> {
        if let Some(result) = self.receive_tasks_if_needed() {
            return result;
        }

        match Pin::new(&mut self.guards).poll_next(cx) {
            // All background tasks are still running.
            Poll::Pending => Ok(()),

            Poll::Ready(Some(res)) => {
                info!(
                    background_tasks = %self.guards.len(),
                    "a peer set background task exited, shutting down other peer set tasks"
                );

                self.shut_down_tasks_and_channels();

                // Flatten the join result and inner result,
                // then turn Ok() task exits into errors.
                res.map_err(Into::into)
                    // TODO: replace with Result::flatten when it stabilises (#70142)
                    .and_then(convert::identity)
                    .and(Err("a peer set background task exited".into()))
            }

            Poll::Ready(None) => {
                self.shut_down_tasks_and_channels();
                Err("all peer set background tasks have exited".into())
            }
        }
    }

    /// Receive background tasks, if they've been sent on the channel,
    /// but not consumed yet.
    ///
    /// Returns a result representing the current task state,
    /// or `None` if the background tasks should be polled to check their state.
    fn receive_tasks_if_needed(&mut self) -> Option<Result<(), BoxError>> {
        if self.guards.is_empty() {
            match self.handle_rx.try_recv() {
                // The tasks haven't been sent yet.
                Err(TryRecvError::Empty) => Some(Ok(())),

                // The tasks have been sent, but not consumed.
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

                    None
                }

                // The tasks have been sent and consumed, but then they exited.
                //
                // Correctness: the peer set must receive at least one task.
                //
                // TODO: refactor `handle_rx` and `guards` into an enum
                //       for the background task state: Waiting/Running/Shutdown.
                Err(TryRecvError::Closed) => {
                    Some(Err("all peer set background tasks have exited".into()))
                }
            }
        } else {
            None
        }
    }

    /// Shut down:
    /// - services by dropping the service lists
    /// - background tasks via their join handles or cancel handles
    /// - channels by closing the channel
    fn shut_down_tasks_and_channels(&mut self) {
        // Drop services and cancel their background tasks.
        self.preselected_p2c_peer = None;
        self.ready_services = HashMap::new();

        for (_peer_key, handle) in self.cancel_handles.drain() {
            let _ = handle.send(CancelClientWork);
        }
        self.unready_services = FuturesUnordered::new();

        // Close the MorePeers channel for all senders,
        // so we don't add more peers to a shut down peer set.
        self.demand_signal.close_channel();

        // Shut down background tasks.
        self.handle_rx.close();
        self.receive_tasks_if_needed();
        for guard in self.guards.iter() {
            guard.abort();
        }
    }

    /// Check busy peer services for request completion or errors.
    ///
    /// Move newly ready services to the ready list if they are for peers with supported protocol
    /// versions, otherwise they are dropped. Also drop failed services.
    fn poll_unready(&mut self, cx: &mut Context<'_>) {
        loop {
            match Pin::new(&mut self.unready_services).poll_next(cx) {
                // No unready service changes, or empty unready services
                Poll::Pending | Poll::Ready(None) => return,

                // Unready -> Ready
                Poll::Ready(Some(Ok((key, svc)))) => {
                    trace!(?key, "service became ready");
                    let cancel = self.cancel_handles.remove(&key);
                    assert!(cancel.is_some(), "missing cancel handle");

                    if svc.remote_version() >= self.minimum_peer_version.current() {
                        self.ready_services.insert(key, svc);
                    }
                }

                // Unready -> Canceled
                Poll::Ready(Some(Err((key, UnreadyError::Canceled)))) => {
                    // A service be canceled because we've connected to the same service twice.
                    // In that case, there is a cancel handle for the peer address,
                    // but it belongs to the service for the newer connection.
                    trace!(
                        ?key,
                        duplicate_connection = self.cancel_handles.contains_key(&key),
                        "service was canceled, dropping service"
                    );
                }
                Poll::Ready(Some(Err((key, UnreadyError::CancelHandleDropped(_))))) => {
                    // Similarly, services with dropped cancel handes can have duplicates.
                    trace!(
                        ?key,
                        duplicate_connection = self.cancel_handles.contains_key(&key),
                        "cancel handle was dropped, dropping service"
                    );
                }

                // Unready -> Errored
                Poll::Ready(Some(Err((key, UnreadyError::Inner(error))))) => {
                    debug!(%error, "service failed while unready, dropping service");

                    let cancel = self.cancel_handles.remove(&key);
                    assert!(cancel.is_some(), "missing cancel handle");
                }
            }
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

    /// Checks for newly inserted or removed services.
    ///
    /// Puts inserted services in the unready list.
    /// Drops removed services, after cancelling any pending requests.
    fn poll_discover(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        use futures::ready;
        loop {
            match ready!(Pin::new(&mut self.discover).poll_discover(cx))
                .ok_or("discovery stream closed")?
                .map_err(Into::into)?
            {
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

                    self.push_unready(key, svc);
                }
            }
        }
    }

    /// Checks if the minimum peer version has changed, and disconnects from outdated peers.
    fn disconnect_from_outdated_peers(&mut self) {
        if let Some(minimum_version) = self.minimum_peer_version.changed() {
            self.ready_services.retain(|address, peer| {
                if peer.remote_version() >= minimum_version {
                    true
                } else {
                    if self.preselected_p2c_peer == Some(*address) {
                        self.preselected_p2c_peer = None;
                    }

                    false
                }
            });
        }
    }

    /// Takes a ready service by key, invalidating `preselected_p2c_peer` if needed.
    fn take_ready_service(&mut self, key: &D::Key) -> Option<D::Service> {
        if let Some(svc) = self.ready_services.remove(key) {
            if Some(*key) == self.preselected_p2c_peer {
                self.preselected_p2c_peer = None;
            }

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
    fn preselect_p2c_peer(&self) -> Option<D::Key> {
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
                // If there are only 2 peers, randomise their order.
                // Otherwise, choose 2 random peers in a random order.
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
        let preselected_key = self
            .preselected_p2c_peer
            .expect("ready peer service must have a preselected peer");

        tracing::trace!(?preselected_key, "routing based on p2c");

        let mut svc = self
            .take_ready_service(&preselected_key)
            .expect("ready peer set must have preselected a ready peer");

        let fut = svc.call(req);
        self.push_unready(preselected_key, svc);
        fut.map_err(Into::into).boxed()
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

        // # Security
        //
        // We choose peers randomly, ignoring load.
        // This avoids favouring malicious peers, because peers can influence their own load.
        //
        // The order of peers isn't completely random,
        // but peer request order is not security-sensitive.

        let futs = FuturesUnordered::new();
        for key in self.select_random_ready_peers(max_peers) {
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

    /// Given a number of ready peers calculate to how many of them Zebra will
    /// actually send the request to. Return this number.
    pub(crate) fn number_of_peers_to_broadcast(&self) -> usize {
        // We are currently sending broadcast messages to a third of the total peers.
        const PEER_FRACTION_TO_BROADCAST: usize = 3;

        // Round up, so that if we have one ready peer, it gets the request.
        div_ceil(self.ready_services.len(), PEER_FRACTION_TO_BROADCAST)
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
        metrics::gauge!("pool.num_ready", num_ready as f64);
        metrics::gauge!("pool.num_unready", num_unready as f64);
        metrics::gauge!("zcash.net.peers", num_peers as f64);

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
        self.poll_background_errors(cx)?;

        // Update peer statuses
        let _ = self.poll_discover(cx)?;
        self.disconnect_from_outdated_peers();
        self.inventory_registry.poll_inventory(cx)?;
        self.poll_unready(cx);

        self.log_peer_set_size();
        self.update_metrics();

        loop {
            // Re-check that the pre-selected service is ready, in case
            // something has happened since (e.g., it failed, peer closed
            // connection, ...)
            if let Some(key) = self.preselected_p2c_peer {
                trace!(preselected_key = ?key);
                let mut service = self
                    .take_ready_service(&key)
                    .expect("preselected peer must be in the ready list");
                match service.poll_ready(cx) {
                    Poll::Ready(Ok(())) => {
                        trace!("preselected service is still ready, keeping it selected");
                        self.preselected_p2c_peer = Some(key);
                        self.ready_services.insert(key, service);
                        return Poll::Ready(Ok(()));
                    }
                    Poll::Pending => {
                        trace!("preselected service is no longer ready, moving to unready list");
                        self.push_unready(key, service);
                    }
                    Poll::Ready(Err(error)) => {
                        trace!(%error, "preselected service failed, dropping it");
                        std::mem::drop(service);
                    }
                }
            }

            trace!("preselected service was not ready, preselecting another ready service");
            self.preselected_p2c_peer = self.preselect_p2c_peer();
            self.update_metrics();

            if self.preselected_p2c_peer.is_none() {
                // CORRECTNESS
                //
                // If the channel is full, drop the demand signal rather than waiting.
                // If we waited here, the crawler could deadlock sending a request to
                // fetch more peers, because it also empties the channel.
                trace!("no ready services, sending demand signal");
                let _ = self.demand_signal.try_send(MorePeers);

                // CORRECTNESS
                //
                // The current task must be scheduled for wakeup every time we
                // return `Poll::Pending`.
                //
                // As long as there are unready or new peers, this task will run,
                // because:
                // - `poll_discover` schedules this task for wakeup when new
                //   peers arrive.
                // - if there are unready peers, `poll_unready` schedules this
                //   task for wakeup when peer services become ready.
                // - if the preselected peer is not ready, `service.poll_ready`
                //   schedules this task for wakeup when that service becomes
                //   ready.
                //
                // To avoid peers blocking on a full background error channel:
                // - if no background tasks have exited since the last poll,
                //   `poll_background_errors` schedules this task for wakeup when
                //   the next task exits.
                return Poll::Pending;
            }
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

            // Choose a random less-loaded peer for all other requests
            _ => self.route_p2c(req),
        };
        self.update_metrics();

        fut
    }
}
