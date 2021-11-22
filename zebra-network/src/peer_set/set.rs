//! Abstractions that represent "the rest of the network".
//!
//! # Implementation
//!
//! The [`PeerSet`] implementation is adapted from the one in the [Tower Balance][tower-balance] crate.
//! As described in that crate's documentation, it:
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
//! [tower-balance]: https://crates.io/crates/tower-balance

use std::{
    collections::HashMap,
    convert,
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

use futures::{
    channel::{mpsc, oneshot},
    future::TryFutureExt,
    prelude::*,
    stream::FuturesUnordered,
};
use indexmap::IndexMap;
use tokio::{
    sync::{broadcast, oneshot::error::TryRecvError},
    task::JoinHandle,
};
use tower::{
    discover::{Change, Discover},
    load::Load,
    Service,
};

use crate::{
    peer_set::{
        unready_service::{Error as UnreadyError, UnreadyService},
        InventoryRegistry,
    },
    protocol::{
        external::InventoryHash,
        internal::{Request, Response},
    },
    AddressBook, BoxError, Config,
};

/// A signal sent by the [`PeerSet`] when it has no ready peers, and gets a request from Zebra.
///
/// In response to this signal, the crawler tries to open more peer connections.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct MorePeers;

/// A signal sent by the [`PeerSet`] to cancel a [`Client`]'s current request or response.
///
/// When it receives this signal, the [`Client`] stops processing and exits.
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
pub struct PeerSet<D>
where
    D: Discover<Key = SocketAddr> + Unpin,
    D::Service: Service<Request, Response = Response> + Load,
    D::Error: Into<BoxError>,
    <D::Service as Service<Request>>::Error: Into<BoxError> + 'static,
    <D::Service as Service<Request>>::Future: Send + 'static,
    <D::Service as Load>::Metric: Debug,
{
    /// Provides new and deleted peer [`Change`]s to the peer set,
    /// via the [`Discover`] trait implementation.
    discover: D,

    /// Connected peers that are ready to receive requests from Zebra,
    /// or send requests to Zebra.
    ready_services: IndexMap<D::Key, D::Service>,

    /// A preselected index for a ready service.
    /// INVARIANT: If this is `Some(i)`, `i` must be a valid index for `ready_services`.
    /// This means that every change to `ready_services` must invalidate or correct it.
    preselected_p2c_index: Option<usize>,

    /// Stores gossiped inventory from connected peers.
    /// Used to route inventory requests to peers that are likely to have it.
    inventory_registry: InventoryRegistry,

    /// Connected peers that are handling a Zebra request,
    /// or Zebra is handling one of their requests.
    unready_services: FuturesUnordered<UnreadyService<D::Key, D::Service, Request>>,

    /// Channels used to cancel the request that an unready service is doing.
    cancel_handles: HashMap<D::Key, oneshot::Sender<CancelClientWork>>,

    /// A channel that asks the peer crawler task to connect to more peers.
    demand_signal: mpsc::Sender<MorePeers>,

    /// Channel for passing ownership of tokio JoinHandles from PeerSet's background tasks
    ///
    /// The join handles passed into the PeerSet are used populate the `guards` member
    handle_rx: tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxError>>>>,

    /// Unordered set of handles to background tasks associated with the `PeerSet`
    ///
    /// These guards are checked for errors as part of `poll_ready` which lets
    /// the `PeerSet` propagate errors from background tasks back to the user
    guards: futures::stream::FuturesUnordered<JoinHandle<Result<(), BoxError>>>,

    /// A shared list of peer addresses.
    ///
    /// Used for logging diagnostics.
    address_book: Arc<std::sync::Mutex<AddressBook>>,

    /// The last time we logged a message about the peer set size
    last_peer_log: Option<Instant>,

    /// The configured limit for inbound and outbound connections.
    ///
    /// The peer set panics if this size is exceeded.
    /// If that happens, our connection limit code has a bug.
    peerset_total_connection_limit: usize,
}

impl<D> Drop for PeerSet<D>
where
    D: Discover<Key = SocketAddr> + Unpin,
    D::Service: Service<Request, Response = Response> + Load,
    D::Error: Into<BoxError>,
    <D::Service as Service<Request>>::Error: Into<BoxError> + 'static,
    <D::Service as Service<Request>>::Future: Send + 'static,
    <D::Service as Load>::Metric: Debug,
{
    fn drop(&mut self) {
        self.shut_down_tasks_and_channels()
    }
}

impl<D> PeerSet<D>
where
    D: Discover<Key = SocketAddr> + Unpin,
    D::Service: Service<Request, Response = Response> + Load,
    D::Error: Into<BoxError>,
    <D::Service as Service<Request>>::Error: Into<BoxError> + 'static,
    <D::Service as Service<Request>>::Future: Send + 'static,
    <D::Service as Load>::Metric: Debug,
{
    /// Construct a peerset which uses `discover` to manage peer connections.
    ///
    /// Arguments:
    /// - `config`: configures the peer set connection limit;
    /// - `discover`: handles peer connects and disconnects;
    /// - `demand_signal`: requests more peers when all peers are busy (unready);
    /// - `handle_rx`: receives background task handles,
    ///                monitors them to make sure they're still running,
    ///                and shuts down all the tasks as soon as one task exits;
    /// - `inv_stream`: receives inventory advertisements for peers,
    ///                 allowing the peer set to direct inventory requests;
    /// - `address_book`: when peer set is busy, it logs address book diagnostics.
    pub fn new(
        config: &Config,
        discover: D,
        demand_signal: mpsc::Sender<MorePeers>,
        handle_rx: tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxError>>>>,
        inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>,
        address_book: Arc<std::sync::Mutex<AddressBook>>,
    ) -> Self {
        Self {
            // Ready peers
            discover,
            ready_services: IndexMap::new(),
            preselected_p2c_index: None,
            inventory_registry: InventoryRegistry::new(inv_stream),

            // Unready peers
            unready_services: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),
            demand_signal,

            // Background tasks
            handle_rx,
            guards: futures::stream::FuturesUnordered::new(),

            // Metrics
            last_peer_log: None,
            address_book,
            peerset_total_connection_limit: config.peerset_total_connection_limit(),
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

                    for handle in handles {
                        self.guards.push(handle);
                    }

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
        self.preselected_p2c_index = None;
        self.ready_services = IndexMap::new();

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

        // TODO: implement graceful shutdown for InventoryRegistry (#1678)
    }

    fn poll_unready(&mut self, cx: &mut Context<'_>) {
        loop {
            match Pin::new(&mut self.unready_services).poll_next(cx) {
                Poll::Pending | Poll::Ready(None) => return,
                Poll::Ready(Some(Ok((key, svc)))) => {
                    trace!(?key, "service became ready");
                    let _cancel = self.cancel_handles.remove(&key);
                    assert!(_cancel.is_some(), "missing cancel handle");
                    self.ready_services.insert(key, svc);
                }
                Poll::Ready(Some(Err((key, UnreadyError::Canceled)))) => {
                    trace!(?key, "service was canceled");
                    // This debug assert is invalid because we can have a
                    // service be canceled due us connecting to the same service
                    // twice.
                    //
                    // assert!(!self.cancel_handles.contains_key(&key))
                }
                Poll::Ready(Some(Err((key, UnreadyError::Inner(e))))) => {
                    let error = e.into();
                    debug!(%error, "service failed while unready, dropped");
                    let _cancel = self.cancel_handles.remove(&key);
                    assert!(_cancel.is_some(), "missing cancel handle");
                }
            }
        }
    }

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
                    trace!(?key, "got Change::Insert from Discover");
                    self.remove(&key);
                    self.push_unready(key, svc);
                }
            }
        }
    }

    /// Takes a ready service by key, preserving `preselected_p2c_index` if possible.
    fn take_ready_service(&mut self, key: &D::Key) -> Option<(D::Key, D::Service)> {
        if let Some((i, key, svc)) = self.ready_services.swap_remove_full(key) {
            // swap_remove perturbs the position of the last element of
            // ready_services, so we may have invalidated self.next_idx, in
            // which case we need to fix it. Specifically, swap_remove swaps the
            // position of the removee and the last element, then drops the
            // removee from the end, so we compare the active and removed indices:
            //
            // We just removed one element, so this was the index of the last element.
            let last_idx = self.ready_services.len();
            self.preselected_p2c_index = match self.preselected_p2c_index {
                None => None,                        // No active index
                Some(j) if j == i => None,           // We removed j
                Some(j) if j == last_idx => Some(i), // We swapped i and j
                Some(j) => Some(j),                  // We swapped an unrelated service.
            };
            // No Heisenservices: they must be ready or unready.
            assert!(!self.cancel_handles.contains_key(&key));
            Some((key, svc))
        } else {
            None
        }
    }

    fn remove(&mut self, key: &D::Key) {
        if self.take_ready_service(key).is_some() {
        } else if let Some(handle) = self.cancel_handles.remove(key) {
            let _ = handle.send(CancelClientWork);
        }
    }

    fn push_unready(&mut self, key: D::Key, svc: D::Service) {
        let (tx, rx) = oneshot::channel();
        self.cancel_handles.insert(key, tx);
        self.unready_services.push(UnreadyService {
            key: Some(key),
            service: Some(svc),
            cancel: rx,
            _req: PhantomData,
        });
    }

    /// Performs P2C on inner services to select a ready service.
    fn preselect_p2c_index(&mut self) -> Option<usize> {
        match self.ready_services.len() {
            0 => None,
            1 => Some(0),
            len => {
                let (a, b) = {
                    let idxs = rand::seq::index::sample(&mut rand::thread_rng(), len, 2);
                    (idxs.index(0), idxs.index(1))
                };

                let a_load = self.query_load(a);
                let b_load = self.query_load(b);

                let selected = if a_load <= b_load { a } else { b };

                trace!(a.idx = a, a.load = ?a_load, b.idx = b, b.load = ?b_load, selected, "selected service by p2c");

                Some(selected)
            }
        }
    }

    /// Accesses a ready endpoint by index and returns its current load.
    fn query_load(&self, index: usize) -> <D::Service as Load>::Metric {
        let (_, svc) = self.ready_services.get_index(index).expect("invalid index");
        svc.load()
    }

    /// Routes a request using P2C load-balancing.
    fn route_p2c(&mut self, req: Request) -> <Self as tower::Service<Request>>::Future {
        let index = self
            .preselected_p2c_index
            .take()
            .expect("ready service must have valid preselected index");

        let (key, mut svc) = self
            .ready_services
            .swap_remove_index(index)
            .expect("preselected index must be valid");

        let fut = svc.call(req);
        self.push_unready(key, svc);
        fut.map_err(Into::into).boxed()
    }

    /// Tries to route a request to a peer that advertised that inventory,
    /// falling back to P2C if there is no ready peer.
    fn route_inv(
        &mut self,
        req: Request,
        hash: InventoryHash,
    ) -> <Self as tower::Service<Request>>::Future {
        let peer = self
            .inventory_registry
            .peers(&hash)
            .find(|&key| self.ready_services.contains_key(key))
            .cloned();

        match peer.and_then(|key| self.take_ready_service(&key)) {
            Some((key, mut svc)) => {
                tracing::debug!(?hash, ?key, "routing based on inventory");
                let fut = svc.call(req);
                self.push_unready(key, svc);
                fut.map_err(Into::into).boxed()
            }
            None => {
                tracing::debug!(?hash, "no ready peer for inventory, falling back to p2c");
                self.route_p2c(req)
            }
        }
    }

    // Routes a request to all ready peers, ignoring return values.
    fn route_all(&mut self, req: Request) -> <Self as tower::Service<Request>>::Future {
        // This is not needless: otherwise, we'd hold a &mut reference to self.ready_services,
        // blocking us from passing &mut self to push_unready.
        let ready_services = std::mem::take(&mut self.ready_services);
        self.preselected_p2c_index = None; // All services are now unready.

        let futs = FuturesUnordered::new();
        for (key, mut svc) in ready_services {
            futs.push(svc.call(req.clone()).map_err(|_| ()));
            self.push_unready(key, svc);
        }

        async move {
            let results = futs.collect::<Vec<Result<_, _>>>().await;
            tracing::debug!(
                ok.len = results.iter().filter(|r| r.is_ok()).count(),
                err.len = results.iter().filter(|r| r.is_err()).count(),
            );
            Ok(Response::Nil)
        }
        .boxed()
    }

    fn log_peer_set_size(&mut self) {
        let ready_services_len = self.ready_services.len();
        let unready_services_len = self.unready_services.len();
        trace!(ready_peers = ?ready_services_len, unready_peers = ?unready_services_len);

        if ready_services_len > 0 {
            return;
        }

        // These logs are designed to be human-readable in a terminal, at the
        // default Zebra log level. If you need to know the peer set size for
        // every request, use the trace-level logs, or the metrics exporter.
        if let Some(last_peer_log) = self.last_peer_log {
            // Avoid duplicate peer set logs
            if Instant::now().duration_since(last_peer_log).as_secs() < 60 {
                return;
            }
        } else {
            // Suppress initial logs until the peer set has started up.
            // There can be multiple initial requests before the first peer is
            // ready.
            self.last_peer_log = Some(Instant::now());
            return;
        }

        self.last_peer_log = Some(Instant::now());

        // # Correctness
        //
        // Only log address metrics in exceptional circumstances, to avoid lock contention.
        //
        // TODO: replace with a watch channel that is updated in `AddressBook::update_metrics()`,
        //       or turn the address book into a service (#1976)
        let address_metrics = self.address_book.lock().unwrap().address_metrics();
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

    fn update_metrics(&self) {
        let num_ready = self.ready_services.len();
        let num_unready = self.unready_services.len();
        let num_peers = num_ready + num_unready;
        metrics::gauge!("pool.num_ready", num_ready as f64);
        metrics::gauge!("pool.num_unready", num_unready as f64);
        metrics::gauge!("zcash.net.peers", num_peers as f64);

        // Security: make sure we haven't exceeded the connection limit
        if num_peers > self.peerset_total_connection_limit {
            let address_metrics = self.address_book.lock().unwrap().address_metrics();
            panic!(
                "unexpectedly exceeded configured peer set connection limit: \n\
                 peers: {:?}, ready: {:?}, unready: {:?}, \n\
                 address_metrics: {:?}",
                num_peers, num_ready, num_unready, address_metrics,
            );
        }
    }
}

impl<D> Service<Request> for PeerSet<D>
where
    D: Discover<Key = SocketAddr> + Unpin,
    D::Service: Service<Request, Response = Response> + Load,
    D::Error: Into<BoxError>,
    <D::Service as Service<Request>>::Error: Into<BoxError> + 'static,
    <D::Service as Service<Request>>::Future: Send + 'static,
    <D::Service as Load>::Metric: Debug,
{
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_background_errors(cx)?;
        // Process peer discovery updates.
        let _ = self.poll_discover(cx)?;
        self.inventory_registry.poll_inventory(cx)?;
        self.poll_unready(cx);

        self.log_peer_set_size();
        self.update_metrics();

        loop {
            // Re-check that the pre-selected service is ready, in case
            // something has happened since (e.g., it failed, peer closed
            // connection, ...)
            if let Some(index) = self.preselected_p2c_index {
                let (key, service) = self
                    .ready_services
                    .get_index_mut(index)
                    .expect("preselected index must be valid");
                trace!(preselected_index = index, ?key);
                match service.poll_ready(cx) {
                    Poll::Ready(Ok(())) => return Poll::Ready(Ok(())),
                    Poll::Pending => {
                        trace!("preselected service is no longer ready");
                        let (key, service) = self
                            .ready_services
                            .swap_remove_index(index)
                            .expect("preselected index must be valid");
                        self.push_unready(key, service);
                    }
                    Poll::Ready(Err(e)) => {
                        let error = e.into();
                        trace!(%error, "preselected service failed, dropping it");
                        self.ready_services
                            .swap_remove_index(index)
                            .expect("preselected index must be valid");
                    }
                }
            }

            trace!("preselected service was not ready, reselecting");
            self.preselected_p2c_index = self.preselect_p2c_index();
            self.update_metrics();

            if self.preselected_p2c_index.is_none() {
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

            // Broadcast advertisements to all peers
            Request::AdvertiseTransactionIds(_) => self.route_all(req),
            Request::AdvertiseBlock(_) => self.route_all(req),

            // Choose a random less-loaded peer for all other requests
            _ => self.route_p2c(req),
        };
        self.update_metrics();

        fut
    }
}
