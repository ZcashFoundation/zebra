use super::{
    future::ResponseFuture,
    message::Message,
    worker::{Handle, Worker},
    BatchControl,
};

use crate::semaphore::Semaphore;
use futures_core::ready;
use std::{
    fmt,
    task::{Context, Poll},
};
use tokio::sync::{mpsc, oneshot};
use tower::Service;

/// Allows batch processing of requests.
///
/// See the module documentation for more details.
pub struct Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
{
    // Note: this actually _is_ bounded, but rather than using Tokio's unbounded
    // channel, we use tokio's semaphore separately to implement the bound.
    tx: mpsc::UnboundedSender<Message<Request, T::Future>>,
    // When the buffer's channel is full, we want to exert backpressure in
    // `poll_ready`, so that callers such as load balancers could choose to call
    // another service rather than waiting for buffer capacity.
    //
    // Unfortunately, this can't be done easily using Tokio's bounded MPSC
    // channel, because it doesn't expose a polling-based interface, only an
    // `async fn ready`, which borrows the sender. Therefore, we implement our
    // own bounded MPSC on top of the unbounded channel, using a semaphore to
    // limit how many items are in the channel.
    semaphore: Semaphore,
    handle: Handle,
}

impl<T, Request> fmt::Debug for Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = std::any::type_name::<Self>();
        f.debug_struct(name)
            .field("tx", &self.tx)
            .field("semaphore", &self.semaphore)
            .field("handle", &self.handle)
            .finish()
    }
}

impl<T, Request> Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Error: Into<crate::BoxError>,
{
    /// Creates a new `Batch` wrapping `service`.
    ///
    /// The wrapper is responsible for telling the inner service when to flush a
    /// batch of requests.  Two parameters control this policy:
    ///
    /// * `max_items` gives the maximum number of items per batch.
    /// * `max_latency` gives the maximum latency for a batch item.
    ///
    /// The default Tokio executor is used to run the given service, which means
    /// that this method must be called while on the Tokio runtime.
    pub fn new(service: T, max_items: usize, max_latency: std::time::Duration) -> Self
    where
        T: Send + 'static,
        T::Future: Send,
        T::Error: Send + Sync,
        Request: Send + 'static,
    {
        // The semaphore bound limits the maximum number of concurrent requests
        // (specifically, requests which got a `Ready` from `poll_ready`, but haven't
        // used their semaphore reservation in a `call` yet).
        // We choose a bound that allows callers to check readiness for every item in
        // a batch, then actually submit those items.
        let bound = max_items;
        let (tx, rx) = mpsc::unbounded_channel();
        let (handle, worker) = Worker::new(service, rx, max_items, max_latency);
        tokio::spawn(worker.run());
        let semaphore = Semaphore::new(bound);
        Batch {
            tx,
            semaphore,
            handle,
        }
    }

    fn get_worker_error(&self) -> crate::BoxError {
        self.handle.get_error_on_closed()
    }
}

impl<T, Request> Service<Request> for Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Error: Into<crate::BoxError>,
{
    type Response = T::Response;
    type Error = crate::BoxError;
    type Future = ResponseFuture<T::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // First, check if the worker is still alive.
        if self.tx.is_closed() {
            // If the inner service has errored, then we error here.
            return Poll::Ready(Err(self.get_worker_error()));
        }

        // Then, poll to acquire a semaphore permit. If we acquire a permit,
        // then there's enough buffer capacity to send a new request. Otherwise,
        // we need to wait for capacity.

        // In tokio 0.3.7, `acquire_owned` panics if its semaphore returns an error,
        // so we don't need to handle errors until we upgrade to tokio 1.0.
        ready!(self.semaphore.poll_acquire(cx));

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        tracing::trace!("sending request to buffer worker");
        let _permit = self
            .semaphore
            .take_permit()
            .expect("buffer full; poll_ready must be called first");

        // get the current Span so that we can explicitly propagate it to the worker
        // if we didn't do this, events on the worker related to this span wouldn't be counted
        // towards that span since the worker would have no way of entering it.
        let span = tracing::Span::current();

        // If we've made it here, then a semaphore permit has already been
        // acquired, so we can freely allocate a oneshot.
        let (tx, rx) = oneshot::channel();

        match self.tx.send(Message {
            request,
            tx,
            span,
            _permit,
        }) {
            Err(_) => ResponseFuture::failed(self.get_worker_error()),
            Ok(_) => ResponseFuture::new(rx),
        }
    }
}

impl<T, Request> Clone for Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
{
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            handle: self.handle.clone(),
            semaphore: self.semaphore.clone(),
        }
    }
}
