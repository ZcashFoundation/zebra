//! Wrapper service for batching items to an underlying service.

use std::{
    cmp::max,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures_core::ready;
use tokio::{
    pin,
    sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::PollSemaphore;
use tower::Service;
use tracing::{info_span, Instrument};

use crate::RequestWeight;

use super::{
    future::ResponseFuture,
    message::Message,
    worker::{ErrorHandle, Worker},
    BatchControl,
};

/// The maximum number of batches in the queue.
///
/// This avoids having very large queues on machines with hundreds or thousands of cores.
pub const QUEUE_BATCH_LIMIT: usize = 64;

/// Allows batch processing of requests.
///
/// See the crate documentation for more details.
pub struct Batch<T, Request: RequestWeight>
where
    T: Service<BatchControl<Request>>,
{
    // Batch management
    //
    /// A custom-bounded channel for sending requests to the batch worker.
    ///
    /// Note: this actually _is_ bounded, but rather than using Tokio's unbounded
    /// channel, we use tokio's semaphore separately to implement the bound.
    tx: mpsc::UnboundedSender<Message<Request, T::Future>>,

    /// A semaphore used to bound the channel.
    ///
    /// When the buffer's channel is full, we want to exert backpressure in
    /// `poll_ready`, so that callers such as load balancers could choose to call
    /// another service rather than waiting for buffer capacity.
    ///
    /// Unfortunately, this can't be done easily using Tokio's bounded MPSC
    /// channel, because it doesn't wake pending tasks on close. Therefore, we implement our
    /// own bounded MPSC on top of the unbounded channel, using a semaphore to
    /// limit how many items are in the channel.
    semaphore: PollSemaphore,

    /// A semaphore permit that allows this service to send one message on `tx`.
    permit: Option<OwnedSemaphorePermit>,

    // Errors
    //
    /// An error handle shared between all service clones for the same worker.
    error_handle: ErrorHandle,

    /// A worker task handle shared between all service clones for the same worker.
    ///
    /// Only used when the worker is spawned on the tokio runtime.
    worker_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl<T, Request: RequestWeight> fmt::Debug for Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = std::any::type_name::<Self>();
        f.debug_struct(name)
            .field("tx", &self.tx)
            .field("semaphore", &self.semaphore)
            .field("permit", &self.permit)
            .field("error_handle", &self.error_handle)
            .field("worker_handle", &self.worker_handle)
            .finish()
    }
}

impl<T, Request: RequestWeight> Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Future: Send + 'static,
    T::Error: Into<crate::BoxError>,
{
    /// Creates a new `Batch` wrapping `service`.
    ///
    /// The wrapper is responsible for telling the inner service when to flush a
    /// batch of requests. These parameters control this policy:
    ///
    /// * `max_items_weight_in_batch` gives the maximum item weight per batch.
    /// * `max_batches` is an upper bound on the number of batches in the queue,
    ///   and the number of concurrently executing batches.
    ///   If this is `None`, we use the current number of [`rayon`] threads.
    ///   The number of batches in the queue is also limited by [`QUEUE_BATCH_LIMIT`].
    /// * `max_latency` gives the maximum latency for a batch item to start verifying.
    ///
    /// The default Tokio executor is used to run the given service, which means
    /// that this method must be called while on the Tokio runtime.
    pub fn new(
        service: T,
        max_items_weight_in_batch: usize,
        max_batches: impl Into<Option<usize>>,
        max_latency: std::time::Duration,
    ) -> Self
    where
        T: Send + 'static,
        T::Future: Send,
        T::Response: Send,
        T::Error: Send + Sync,
        Request: Send + 'static,
    {
        let (mut batch, worker) =
            Self::pair(service, max_items_weight_in_batch, max_batches, max_latency);

        let span = info_span!("batch worker", kind = std::any::type_name::<T>());

        #[cfg(tokio_unstable)]
        let worker_handle = {
            let batch_kind = std::any::type_name::<T>();

            // TODO: identify the unique part of the type name generically,
            //       or make it an argument to this method
            let batch_kind = batch_kind.trim_start_matches("zebra_consensus::primitives::");
            let batch_kind = batch_kind.trim_end_matches("::Verifier");

            tokio::task::Builder::new()
                .name(&format!("{} batch", batch_kind))
                .spawn(worker.run().instrument(span))
                .expect("panic on error to match tokio::spawn")
        };
        #[cfg(not(tokio_unstable))]
        let worker_handle = tokio::spawn(worker.run().instrument(span));

        batch.register_worker(worker_handle);

        batch
    }

    /// Creates a new `Batch` wrapping `service`, but returns the background worker.
    ///
    /// This is useful if you do not want to spawn directly onto the `tokio`
    /// runtime but instead want to use your own executor. This will return the
    /// `Batch` and the background `Worker` that you can then spawn.
    pub fn pair(
        service: T,
        max_items_weight_in_batch: usize,
        max_batches: impl Into<Option<usize>>,
        max_latency: std::time::Duration,
    ) -> (Self, Worker<T, Request>)
    where
        T: Send + 'static,
        T::Error: Send + Sync,
        Request: Send + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel();

        // Clamp config to sensible values.
        let max_items_weight_in_batch = max(max_items_weight_in_batch, 1);
        let max_batches = max_batches
            .into()
            .unwrap_or_else(rayon::current_num_threads);
        let max_batches_in_queue = max_batches.clamp(1, QUEUE_BATCH_LIMIT);

        // The semaphore bound limits the maximum number of concurrent requests
        // (specifically, requests which got a `Ready` from `poll_ready`, but haven't
        // used their semaphore reservation in a `call` yet).
        //
        // We choose a bound that allows callers to check readiness for one batch per rayon CPU thread.
        // This helps keep all CPUs filled with work: there is one batch executing, and another ready to go.
        // Often there is only one verifier running, when that happens we want it to take all the cores.
        //
        // Request types that make have a request weight greater than 1 may not exhaust the number of
        // available permits, but the maximum number of concurrent requests being handled will still be bounded
        // to the maximum number of possible requests.
        let semaphore = Semaphore::new(max_items_weight_in_batch * max_batches_in_queue);
        let semaphore = PollSemaphore::new(Arc::new(semaphore));

        let (error_handle, worker) = Worker::new(
            service,
            rx,
            max_items_weight_in_batch,
            max_batches,
            max_latency,
            semaphore.clone(),
        );

        let batch = Batch {
            tx,
            semaphore,
            permit: None,
            error_handle,
            worker_handle: Arc::new(Mutex::new(None)),
        };

        (batch, worker)
    }

    /// Ask the `Batch` to monitor the spawned worker task's [`JoinHandle`].
    ///
    /// Only used when the task is spawned on the tokio runtime.
    pub fn register_worker(&mut self, worker_handle: JoinHandle<()>) {
        *self
            .worker_handle
            .lock()
            .expect("previous task panicked while holding the worker handle mutex") =
            Some(worker_handle);
    }

    /// Returns the error from the batch worker's `error_handle`.
    fn get_worker_error(&self) -> crate::BoxError {
        self.error_handle.get_error_on_closed()
    }
}

impl<T, Request: RequestWeight> Service<Request> for Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Future: Send + 'static,
    T::Error: Into<crate::BoxError>,
{
    type Response = T::Response;
    type Error = crate::BoxError;
    type Future = ResponseFuture<T::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Check to see if the worker has returned or panicked.
        //
        // Correctness: Registers this task for wakeup when the worker finishes.
        if let Some(worker_handle) = self
            .worker_handle
            .lock()
            .expect("previous task panicked while holding the worker handle mutex")
            .as_mut()
        {
            match Pin::new(worker_handle).poll(cx) {
                Poll::Ready(Ok(())) => return Poll::Ready(Err(self.get_worker_error())),
                Poll::Ready(Err(task_cancelled)) if task_cancelled.is_cancelled() => {
                    tracing::warn!(
                        "batch task cancelled: {task_cancelled}\n\
                         Is Zebra shutting down?"
                    );

                    return Poll::Ready(Err(task_cancelled.into()));
                }
                Poll::Ready(Err(task_panic)) => {
                    std::panic::resume_unwind(task_panic.into_panic());
                }
                Poll::Pending => {}
            }
        }

        // Check if the worker has set an error and closed its channels.
        //
        // Correctness: Registers this task for wakeup when the channel is closed.
        let tx = self.tx.clone();
        let closed = tx.closed();
        pin!(closed);
        if closed.poll(cx).is_ready() {
            return Poll::Ready(Err(self.get_worker_error()));
        }

        // Poll to acquire a semaphore permit.
        //
        // CORRECTNESS
        //
        // If we acquire a permit, then there's enough buffer capacity to send a new request.
        // Otherwise, we need to wait for capacity. When that happens, `poll_acquire()` registers
        // this task for wakeup when the next permit is available, or when the semaphore is closed.
        //
        // When `poll_ready()` is called multiple times, and channel capacity is 1,
        // avoid deadlocks by dropping any previous permit before acquiring another one.
        // This also stops tasks holding a permit after an error.
        //
        // Calling `poll_ready()` multiple times can make tasks lose their previous permit
        // to another concurrent task.
        self.permit = None;

        let permit = ready!(self.semaphore.poll_acquire(cx));
        if let Some(permit) = permit {
            // Calling poll_ready() more than once will drop any previous permit,
            // releasing its capacity back to the semaphore.
            self.permit = Some(permit);
        } else {
            // The semaphore has been closed.
            return Poll::Ready(Err(self.get_worker_error()));
        }

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        tracing::trace!("sending request to buffer worker");
        let _permit = self
            .permit
            .take()
            .expect("poll_ready must be called before a batch request");

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

impl<T, Request: RequestWeight> Clone for Batch<T, Request>
where
    T: Service<BatchControl<Request>>,
{
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            semaphore: self.semaphore.clone(),
            permit: None,
            error_handle: self.error_handle.clone(),
            worker_handle: self.worker_handle.clone(),
        }
    }
}
