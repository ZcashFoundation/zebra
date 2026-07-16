//! The `FairBuffer` service wrapper.

use std::{
    fmt,
    hash::Hash,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use tokio::sync::oneshot;
use tower::Service;

use super::{
    future::ResponseFuture,
    message::Message,
    queue::{Push, Shared},
    tagged::Tagged,
    worker::Worker,
    BoxError,
};

/// Adds a priority queue in front of an inner service, ordered by each
/// caller's recent request count.
///
/// See the crate documentation for more details.
pub struct FairBuffer<T, K, R>
where
    T: Service<R>,
{
    /// The queue and state shared with the worker task.
    shared: Arc<Shared<K, R, T::Future>>,
}

impl<T, K, R> FairBuffer<T, K, R>
where
    T: Service<R>,
    T::Future: Send + 'static,
    T::Error: Into<BoxError>,
    K: Clone + Eq + Hash,
    R: Send + 'static,
{
    /// Creates a new [`FairBuffer`] wrapping `service`, and spawns its
    /// [`Worker`] onto the default Tokio executor.
    ///
    /// `capacity` is the maximum number of queued requests: pushing a request
    /// beyond it sheds the queued request whose caller had the highest recent
    /// request count. Internal requests are never shed, so the queue can
    /// exceed `capacity` when it is full of internal requests.
    ///
    /// `rotation_interval` is the period after which recent request counts
    /// decay: a caller's count covers at least one and at most two intervals.
    ///
    /// # Panics
    ///
    /// If `capacity` is zero, or if called outside a Tokio runtime.
    pub fn new(service: T, capacity: usize, rotation_interval: Duration) -> Self
    where
        T: Send + 'static,
        T::Error: Send + Sync,
        K: Send + 'static,
    {
        let (buffer, worker) = Self::pair(service, capacity, rotation_interval);
        tokio::spawn(worker.run());
        buffer
    }

    /// Creates a new [`FairBuffer`] wrapping `service`, returning the
    /// background [`Worker`] to be spawned by the caller.
    ///
    /// See [`FairBuffer::new`] for details.
    ///
    /// # Panics
    ///
    /// If `capacity` is zero.
    pub fn pair(
        service: T,
        capacity: usize,
        rotation_interval: Duration,
    ) -> (Self, Worker<T, K, R>) {
        let shared = Arc::new(Shared::new(capacity));
        let worker = Worker::new(service, shared.clone(), rotation_interval);

        (FairBuffer { shared }, worker)
    }
}

impl<T, K, R> Service<Tagged<K, R>> for FairBuffer<T, K, R>
where
    T: Service<R>,
    T::Future: Send + 'static,
    T::Error: Into<BoxError>,
    K: Clone + Eq + Hash,
    R: Send + 'static,
{
    type Response = T::Response;
    type Error = BoxError;
    type Future = ResponseFuture<K, T::Future>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // The fair buffer is always ready unless its worker has shut down:
        // over-capacity requests are shed instead of exerting backpressure,
        // so there are no slots to reserve, and `tower::buffer`'s
        // reserved-slot hangs can't happen.
        Poll::Ready(self.shared.check_open())
    }

    fn call(&mut self, Tagged { key, request }: Tagged<K, R>) -> Self::Future {
        tracing::trace!("sending request to fair buffer worker");

        // Get the current Span so that we can explicitly propagate it to the
        // worker. If we didn't do this, events on the worker related to this
        // span wouldn't be counted towards that span since the worker would
        // have no way of entering it.
        let span = tracing::Span::current();
        let (tx, rx) = oneshot::channel();

        // The response future records the inner service's response time
        // against the caller's recent cost, so it keeps its own copy of the
        // key.
        match self.shared.push(key.clone(), Message { request, tx, span }) {
            Push::Queued => ResponseFuture::new(rx, key, self.shared.costs()),
            Push::Failed(error) => ResponseFuture::failed(error),
        }
    }
}

impl<T, K, R> Clone for FairBuffer<T, K, R>
where
    T: Service<R>,
{
    fn clone(&self) -> Self {
        self.shared.handle_cloned();

        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T, K, R> Drop for FairBuffer<T, K, R>
where
    T: Service<R>,
{
    fn drop(&mut self) {
        // The worker shuts down once every handle has dropped and the queue
        // has drained, matching `tower::buffer`'s teardown when all senders
        // drop.
        self.shared.handle_dropped();
    }
}

impl<T, K, R> fmt::Debug for FairBuffer<T, K, R>
where
    T: Service<R>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FairBuffer").finish_non_exhaustive()
    }
}
