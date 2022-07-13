//! Batch worker item handling and run loop implementation.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use futures::{
    future::{BoxFuture, OptionFuture},
    stream::FuturesUnordered,
    FutureExt, StreamExt,
};
use pin_project::pin_project;
use tokio::{
    sync::mpsc,
    time::{sleep, Sleep},
};
use tokio_util::sync::PollSemaphore;
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use super::{
    error::{Closed, ServiceError},
    message::{self, Message},
    BatchControl,
};

/// Task that handles processing the buffer. This type should not be used
/// directly, instead `Buffer` requires an `Executor` that can accept this task.
///
/// The struct is `pub` in the private module and the type is *not* re-exported
/// as part of the public API. This is the "sealed" pattern to include "private"
/// types in public traits that are not meant for consumers of the library to
/// implement (only call).
#[pin_project(PinnedDrop)]
#[derive(Debug)]
pub struct Worker<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Future: Send + 'static,
    T::Error: Into<crate::BoxError>,
{
    // Batch management
    //
    /// A semaphore-bounded channel for receiving requests from the batch wrapper service.
    rx: mpsc::UnboundedReceiver<Message<Request, T::Future>>,

    /// The wrapped service that processes batches.
    service: T,

    /// The batches that the worker is concurrently executing.
    concurrent_batches: FuturesUnordered<BoxFuture<'static, Result<T::Response, T::Error>>>,

    // Errors and termination
    //
    /// An error that's populated on permanent service failure.
    failed: Option<ServiceError>,

    /// A shared error handle that's populated on permanent service failure.
    error_handle: ErrorHandle,

    /// A cloned copy of the wrapper service's semaphore, used to close the semaphore.
    close: PollSemaphore,

    // Config
    //
    /// The maximum number of items allowed in a batch.
    max_items_in_batch: usize,

    /// The maximum number of batches that are allowed to run concurrently.
    max_concurrent_batches: usize,

    /// The maximum delay before processing a batch with fewer than `max_items_in_batch`.
    max_latency: std::time::Duration,
}

/// Get the error out
#[derive(Debug)]
pub(crate) struct ErrorHandle {
    inner: Arc<Mutex<Option<ServiceError>>>,
}

impl<T, Request> Worker<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Future: Send + 'static,
    T::Error: Into<crate::BoxError>,
{
    /// Creates a new batch worker.
    ///
    /// See [`Service::new()`](crate::Service::new) for details.
    pub(crate) fn new(
        service: T,
        rx: mpsc::UnboundedReceiver<Message<Request, T::Future>>,
        max_items_in_batch: usize,
        max_concurrent_batches: usize,
        max_latency: std::time::Duration,
        close: PollSemaphore,
    ) -> (ErrorHandle, Worker<T, Request>) {
        let error_handle = ErrorHandle {
            inner: Arc::new(Mutex::new(None)),
        };

        let worker = Worker {
            rx,
            service,
            concurrent_batches: FuturesUnordered::new(),
            failed: None,
            error_handle: error_handle.clone(),
            close,
            max_items_in_batch,
            max_concurrent_batches,
            max_latency,
        };

        (error_handle, worker)
    }

    /// Process a single worker request.
    async fn process_req(&mut self, req: Request, tx: message::Tx<T::Future>) {
        if let Some(ref failed) = self.failed {
            tracing::trace!("notifying batch request caller about worker failure");
            let _ = tx.send(Err(failed.clone()));
            return;
        }

        match self.service.ready().await {
            Ok(svc) => {
                let rsp = svc.call(req.into());
                let _ = tx.send(Ok(rsp));
            }
            Err(e) => {
                self.failed(e.into());
                let _ = tx.send(Err(self
                    .failed
                    .as_ref()
                    .expect("Worker::failed did not set self.failed?")
                    .clone()));
            }
        }
    }

    /// Tell the inner service to flush the current batch.
    ///
    /// Waits until the inner service is ready,
    /// then stores a future which resolves when the batch finishes.
    async fn flush_service(&mut self) {
        if self.failed.is_some() {
            tracing::trace!("worker failure: skipping flush");
            return;
        }

        match self.service.ready().await {
            Ok(ready_service) => {
                let flush_future = ready_service.call(BatchControl::Flush);
                self.concurrent_batches.push(flush_future.boxed());
            }
            Err(error) => {
                self.failed(error.into());
            }
        }
    }

    /* TODO: let other tasks run while we're waiting
       // Correctness: allow other tasks to run at the end of every batch.
       tokio::task::yield_now().await;

    */

    /// Run loop for batch requests, which implements the batch policies.
    ///
    /// See [`Service::new()`](crate::Service::new) for details.
    pub async fn run(mut self) {
        // The timer is started when the first entry of a new batch is
        // submitted, so that the batch latency of all entries is at most
        // self.max_latency. However, we don't keep the timer running unless
        // there is a pending request to prevent wakeups on idle services.
        let mut timer: Option<Pin<Box<Sleep>>> = None;
        let mut pending_items = 0usize;

        loop {
            // Wait on either a new message or the batch timer.
            //
            // If both are ready, end the batch now, because the timer has elapsed.
            // If the timer elapses, any pending messages are preserved:
            // https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.UnboundedReceiver.html#cancel-safety
            tokio::select! {
                biased;

                // The batch timer elapsed.
                Some(()) = OptionFuture::from(timer.as_mut()), if timer.as_ref().is_some() => {
                    tracing::trace!("batch timer expired");

                    // TODO: use a batch-specific span to instrument this future.
                    self.flush_service().await;

                    // Now we have an empty batch.
                    timer = None;
                    pending_items = 0;
                }

                maybe_msg = self.rx.recv() => match maybe_msg {
                    Some(msg) => {
                        tracing::trace!("batch message received");

                        let span = msg.span;

                        self.process_req(msg.request, msg.tx)
                            // Apply the provided span to request processing.
                            .instrument(span)
                            .await;
                        pending_items += 1;

                        // Check whether we have too many pending items.
                        if pending_items >= self.max_items_in_batch {
                            tracing::trace!("batch is full");

                            // TODO: use a batch-specific span to instrument this future.
                            self.flush_service().await;

                            // Now we have an empty batch.
                            timer = None;
                            pending_items = 0;
                        } else if pending_items == 1 {
                            tracing::trace!("batch is new, starting timer");

                            // The first message in a new batch.
                            timer = Some(Box::pin(sleep(self.max_latency)));
                        } else {
                            tracing::trace!("waiting for full batch or batch timer");
                        }
                    }
                    None => {
                        tracing::trace!("batch channel closed and emptied, exiting worker task");

                        return;
                    }
                },
            }
        }
    }

    /// Handle errors from the inner service or the batches it spawns.
    ///
    /// Returns the response, or `None` on error.
    fn handle_error<U>(&mut self, result: Result<U, T::Error>) -> Option<U> {
        match result {
            Ok(response) => Some(response),
            Err(error) => {
                self.failed(error.into());
                None
            }
        }
    }

    /// Register an inner service failure.
    ///
    /// The underlying service failed when we called `poll_ready` on it with the given `error`. We
    /// need to communicate this to all the `Buffer` handles. To do so, we wrap up the error in
    /// an `Arc`, send that `Arc<E>` to all pending requests, and store it so that subsequent
    /// requests will also fail with the same error.
    fn failed(&mut self, error: crate::BoxError) {
        tracing::debug!(?error, "batch worker error");

        // Note that we need to handle the case where some error_handle is concurrently trying to send us
        // a request. We need to make sure that *either* the send of the request fails *or* it
        // receives an error on the `oneshot` it constructed. Specifically, we want to avoid the
        // case where we send errors to all outstanding requests, and *then* the caller sends its
        // request. We do this by *first* exposing the error, *then* closing the channel used to
        // send more requests (so the client will see the error when the send fails), and *then*
        // sending the error to all outstanding requests.
        let error = ServiceError::new(error);

        let mut inner = self.error_handle.inner.lock().unwrap();

        if inner.is_some() {
            // Future::poll was called after we've already errored out!
            return;
        }

        *inner = Some(error.clone());
        drop(inner);

        tracing::trace!("worker failure: waking pending requests so they can be failed");
        self.rx.close();
        self.close.close();

        // By closing the mpsc::Receiver, we know that that the run() loop will
        // drain all pending requests. We just need to make sure that any
        // requests that we receive before we've exhausted the receiver receive
        // the error:
        self.failed = Some(error);
    }
}

impl ErrorHandle {
    pub(crate) fn get_error_on_closed(&self) -> crate::BoxError {
        self.inner
            .lock()
            .expect("previous task panicked while holding the error handle mutex")
            .as_ref()
            .map(|svc_err| svc_err.clone().into())
            .unwrap_or_else(|| Closed::new().into())
    }
}

impl Clone for ErrorHandle {
    fn clone(&self) -> ErrorHandle {
        ErrorHandle {
            inner: self.inner.clone(),
        }
    }
}

#[pin_project::pinned_drop]
impl<T, Request> PinnedDrop for Worker<T, Request>
where
    T: Service<BatchControl<Request>>,
    T::Future: Send + 'static,
    T::Error: Into<crate::BoxError>,
{
    fn drop(mut self: Pin<&mut Self>) {
        tracing::trace!("dropping batch worker");

        // Fail pending tasks
        self.failed(Closed::new().into());

        // Clear queued requests
        while self.rx.try_recv().is_ok() {}

        // Stop accepting reservations
        self.close.close();

        // Clear any finished batches, ignoring any errors.
        // Ignore any batches that are still executing, because we can't cancel them.
        //
        // now_or_never() can stop futures waking up, but that's ok here,
        // because we're manually polling, then dropping the stream.
        while let Some(Some(_)) = self
            .as_mut()
            .project()
            .concurrent_batches
            .next()
            .now_or_never()
        {}
    }
}
