use std::{
    collections::HashMap,
    convert::TryInto,
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
    stream::FuturesUnordered,
};
use indexmap::IndexMap;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::task::JoinHandle;
use tower::{
    discover::{Change, Discover},
    Service,
};
use tower_load::Load;

use crate::{
    protocol::internal::{Request, Response},
    BoxedStdError,
};

use super::unready_service::{Error as UnreadyError, UnreadyService};

/// A [`tower::Service`] that abstractly represents "the rest of the network".
///
/// This implementation is adapted from the one in `tower-balance`, and as
/// described in that crate's documentation, it
///
/// > Distributes requests across inner services using the [Power of Two Choices][p2c].
/// >
/// > As described in the [Finagle Guide][finagle]:
/// >
/// > > The algorithm randomly picks two services from the set of ready endpoints and
/// > > selects the least loaded of the two. By repeatedly using this strategy, we can
/// > > expect a manageable upper bound on the maximum load of any server.
/// > >
/// > > The maximum load variance between any two servers is bound by `ln(ln(n))` where
/// > > `n` is the number of servers in the cluster.
///
/// This should work well for many network requests, but not all of them: some
/// requests, e.g., a request for some particular inventory item, can only be
/// made to a subset of connected peers, e.g., the ones that have recently
/// advertised that inventory hash, and other requests require specialized logic
/// (e.g., transaction diffusion).
///
/// Implementing this specialized routing logic inside the `PeerSet` -- so that
/// it continues to abstract away "the rest of the network" into one endpoint --
/// is not a problem, as the `PeerSet` can simply maintain more information on
/// its peers and route requests appropriately. However, there is a problem with
/// maintaining accurate backpressure information, because the `Service` trait
/// requires that service readiness is independent of the data in the request.
///
/// For this reason, in the future, this code will probably be refactored to
/// address this backpressure mismatch. One possibility is to refactor the code
/// so that one entity holds and maintains the peer set and metadata on the
/// peers, and each "backpressure category" of request is assigned to different
/// `Service` impls with specialized `poll_ready()` implementations. Another
/// less-elegant solution (which might be useful as an intermediate step for the
/// inventory case) is to provide a way to borrow a particular backing service,
/// say by address.
///
/// [finagle]: https://twitter.github.io/finagle/guide/Clients.html#power-of-two-choices-p2c-least-loaded
/// [p2c]: http://www.eecs.harvard.edu/~michaelm/postscripts/handbook2001.pdf
pub struct PeerSet<D>
where
    D: Discover,
{
    discover: D,
    ready_services: IndexMap<D::Key, D::Service>,
    cancel_handles: HashMap<D::Key, oneshot::Sender<()>>,
    unready_services: FuturesUnordered<UnreadyService<D::Key, D::Service, Request>>,
    next_idx: Option<usize>,
    demand_signal: mpsc::Sender<()>,
    /// Channel for passing ownership of tokio JoinHandles from PeerSet's background tasks
    ///
    /// The join handles passed into the PeerSet are used populate the `guards` member
    handle_rx: tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxedStdError>>>>,
    /// Unordered set of handles to background tasks associated with the `PeerSet`
    ///
    /// These guards are checked for errors as part of `poll_ready` which lets
    /// the `PeerSet` propagate errors from background tasks back to the user
    guards: futures::stream::FuturesUnordered<JoinHandle<Result<(), BoxedStdError>>>,
}

impl<D> PeerSet<D>
where
    D: Discover + Unpin,
    D::Key: Clone + Debug,
    D::Service: Service<Request, Response = Response> + Load,
    D::Error: Into<BoxedStdError>,
    <D::Service as Service<Request>>::Error: Into<BoxedStdError> + 'static,
    <D::Service as Service<Request>>::Future: Send + 'static,
    <D::Service as Load>::Metric: Debug,
{
    /// Construct a peerset which uses `discover` internally.
    pub fn new(
        discover: D,
        demand_signal: mpsc::Sender<()>,
        handle_rx: tokio::sync::oneshot::Receiver<Vec<JoinHandle<Result<(), BoxedStdError>>>>,
    ) -> Self {
        Self {
            discover,
            ready_services: IndexMap::new(),
            cancel_handles: HashMap::new(),
            unready_services: FuturesUnordered::new(),
            next_idx: None,
            demand_signal,
            guards: futures::stream::FuturesUnordered::new(),
            handle_rx,
        }
    }

    fn poll_discover(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxedStdError>> {
        use futures::ready;
        loop {
            match ready!(Pin::new(&mut self.discover).poll_discover(cx)).map_err(Into::into)? {
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

    fn remove(&mut self, key: &D::Key) {
        // Remove key from either the set of ready services,
        // or else from the set of unready services.
        if let Some((i, _, _)) = self.ready_services.swap_remove_full(key) {
            // swap_remove perturbs the position of the last element of
            // ready_services, so we may have invalidated self.next_idx, in
            // which case we need to fix it. Specifically, swap_remove swaps the
            // position of the removee and the last element, then drops the
            // removee from the end, so we compare the active and removed indices:
            let len = self.ready_services.len();
            self.next_idx = match self.next_idx {
                None => None,                   // No active index
                Some(j) if j == i => None,      // We removed j
                Some(j) if j == len => Some(i), // We swapped i and j
                Some(j) => Some(j),             // We swapped an unrelated service.
            };
            // No Heisenservices: they must be ready or unready.
            assert!(!self.cancel_handles.contains_key(key));
        } else if let Some(handle) = self.cancel_handles.remove(key) {
            let _ = handle.send(());
        }
    }

    fn push_unready(&mut self, key: D::Key, svc: D::Service) {
        let (tx, rx) = oneshot::channel();
        self.cancel_handles.insert(key.clone(), tx);
        self.unready_services.push(UnreadyService {
            key: Some(key),
            service: Some(svc),
            cancel: rx,
            _req: PhantomData,
        });
    }

    fn check_for_background_errors(&mut self, cx: &mut Context) -> Result<(), BoxedStdError> {
        if self.guards.is_empty() {
            match self.handle_rx.try_recv() {
                Ok(handles) => {
                    for handle in handles {
                        self.guards.push(handle);
                    }
                }
                Err(TryRecvError::Closed) => unreachable!(
                    "try_recv will never be called if the futures have already been received"
                ),
                Err(TryRecvError::Empty) => return Ok(()),
            }
        }

        match Pin::new(&mut self.guards).poll_next(cx) {
            Poll::Pending => {}
            Poll::Ready(Some(res)) => res??,
            Poll::Ready(None) => Err("all background tasks have exited")?,
        }

        Ok(())
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

    /// Performs P2C on inner services to select a ready service.
    fn select_next_ready_index(&mut self) -> Option<usize> {
        match self.ready_services.len() {
            0 => None,
            1 => Some(0),
            len => {
                // XXX avoid relying on rand complexity
                let (a, b) = {
                    let idxs = rand::seq::index::sample(&mut rand::thread_rng(), len, 2);
                    (idxs.index(0), idxs.index(1))
                };

                let a_load = self.ready_index_load(a);
                let b_load = self.ready_index_load(b);

                let selected = if a_load <= b_load { a } else { b };

                trace!(a.idx = a, a.load = ?a_load, b.idx = b, b.load = ?b_load, selected, "selected service by p2c");

                Some(selected)
            }
        }
    }

    /// Accesses a ready endpoint by index and returns its current load.
    fn ready_index_load(&self, index: usize) -> <D::Service as Load>::Metric {
        let (_, svc) = self.ready_services.get_index(index).expect("invalid index");
        svc.load()
    }
}

impl<D> Service<Request> for PeerSet<D>
where
    D: Discover + Unpin,
    D::Key: Clone + Debug + ToString,
    D::Service: Service<Request, Response = Response> + Load,
    D::Error: Into<BoxedStdError>,
    <D::Service as Service<Request>>::Error: Into<BoxedStdError> + 'static,
    <D::Service as Service<Request>>::Future: Send + 'static,
    <D::Service as Load>::Metric: Debug,
{
    type Response = Response;
    type Error = BoxedStdError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.check_for_background_errors(cx)?;
        // Process peer discovery updates.
        let _ = self.poll_discover(cx)?;

        // Poll unready services to drive them to readiness.
        self.poll_unready(cx);
        let num_ready = self.ready_services.len();
        let num_unready = self.unready_services.len();
        metrics::gauge!("pool.num_ready", num_ready.try_into().unwrap(),);
        metrics::gauge!("pool.num_unready", num_unready.try_into().unwrap(),);
        metrics::gauge!(
            "pool.num_peers",
            (num_ready + num_unready).try_into().unwrap(),
        );

        loop {
            // Re-check that the pre-selected service is ready, in case
            // something has happened since (e.g., it failed, peer closed
            // connection, ...)
            if let Some(index) = self.next_idx {
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
            self.next_idx = self.select_next_ready_index();

            if self.next_idx.is_none() {
                trace!("no ready services, sending demand signal");
                let _ = self.demand_signal.try_send(());
                return Poll::Pending;
            }
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let index = self
            .next_idx
            .take()
            .expect("ready service must have valid preselected index");
        let (key, mut svc) = self
            .ready_services
            .swap_remove_index(index)
            .expect("preselected index must be valid");

        // XXX add a dimension tagging request metrics by type
        metrics::counter!(
            "outbound_requests",
            1,
            "key" => key.to_string(),
        );

        let fut = svc.call(req);
        self.push_unready(key, svc);

        use futures::future::TryFutureExt;
        fut.map_err(Into::into).boxed()
    }
}
