//! Wrapper service for batching items to an underlying service.

use super::{
    future::ResponseFuture,
    message::Message,
    worker::{ErrorHandle, Worker},
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
use tracing::{info_span, Instrument};

/// Allows batch processing of requests.
///
/// See the module documentation for more details.
pub struct Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
{
    /// A custom-bounded channel for sending requests to the batch worker.
    ///
    /// Note: this actually _is_ bounded, but rather than using Tokio's unbounded
    /// channel, we use tokio's semaphore separately to implement the bound.
    tx: mpsc::UnboundedSender<Message<Request, T::Future>>,

    /// A semaphore used to bound the channel.
    ///
    /// TODO: replace with bounded mpsc::Sender::reserve() and delete crate::Semaphore
    ///
    /// When the buffer's channel is full, we want to exert backpressure in
    /// `poll_ready`, so that callers such as load balancers could choose to call
    /// another service rather than waiting for buffer capacity.
    ///
    /// Unfortunately, this can't be done easily using Tokio's bounded MPSC
    /// channel, because it doesn't expose a polling-based interface, only an
    /// `async fn ready`, which borrows the sender. Therefore, we implement our
    /// own bounded MPSC on top of the unbounded channel, using a semaphore to
    /// limit how many items are in the channel.
    semaphore: Semaphore,

    /// An error handle shared between all service clones for the same worker.
    error_handle: ErrorHandle,
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
            .field("error_handle", &self.error_handle)
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
        let (batch, worker) = Self::pair(service, max_items, max_latency);

        let span = info_span!("batch worker", kind = std::any::type_name::<T>());

        // TODO: check for panics in the returned JoinHandle (#4738)
        #[cfg(tokio_unstable)]
        let _worker_handle = {
            let batch_kind = std::any::type_name::<T>();

            // TODO: identify the unique part of the type name generically,
            //       or make it an argument to this method
            let batch_kind = batch_kind.trim_start_matches("zebra_consensus::primitives::");
            let batch_kind = batch_kind.trim_end_matches("::Verifier");

            tokio::task::Builder::new()
                .name(&format!("{} batch", batch_kind))
                .spawn(worker.run().instrument(span))
        };
        #[cfg(not(tokio_unstable))]
        let _worker_handle = tokio::spawn(worker.run().instrument(span));

        batch
    }

    /// Creates a new `Batch` wrapping `service`, but returns the background worker.
    ///
    /// This is useful if you do not want to spawn directly onto the `tokio`
    /// runtime but instead want to use your own executor. This will return the
    /// `Batch` and the background `Worker` that you can then spawn.
    pub fn pair(
        service: T,
        max_items: usize,
        max_latency: std::time::Duration,
    ) -> (Self, Worker<T, Request>)
    where
        T: Send + 'static,
        T::Error: Send + Sync,
        Request: Send + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel();

        // The semaphore bound limits the maximum number of concurrent requests
        // (specifically, requests which got a `Ready` from `poll_ready`, but haven't
        // used their semaphore reservation in a `call` yet).
        // We choose a bound that allows callers to check readiness for every item in
        // a batch, then actually submit those items.
        let bound = max_items;
        let (semaphore, close) = Semaphore::new_with_close(bound);

        let (error_handle, worker) = Worker::new(service, rx, max_items, max_latency, close);
        let batch = Batch {
            tx,
            semaphore,
            error_handle,
        };

        (batch, worker)
    }

    fn get_worker_error(&self) -> crate::BoxError {
        self.error_handle.get_error_on_closed()
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

        // CORRECTNESS
        //
        // Poll to acquire a semaphore permit. If we acquire a permit, then
        // there's enough buffer capacity to send a new request. Otherwise, we
        // need to wait for capacity.
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`. If it returns Pending, the semaphore also schedules
        // the task for wakeup when the next permit is available.
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
            error_handle: self.error_handle.clone(),
            semaphore: self.semaphore.clone(),
        }
    }
}
