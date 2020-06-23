use super::{
    future::ResponseFuture,
    message::Message,
    worker::{Handle, Worker},
    BatchControl,
};

use futures_core::ready;
use std::{
    marker::PhantomData,
    task::{Context, Poll},
};
use tokio::sync::{mpsc, oneshot};
use tower::Service;

/// Allows batch processing of requests.
///
/// See the module documentation for more details.
#[derive(Debug)]
pub struct Batch<S, Request, E2 = crate::BoxError>
where
    S: Service<BatchControl<Request>>,
{
    tx: mpsc::Sender<Message<Request, S::Future, S::Error>>,
    handle: Handle<S::Error, E2>,
    _e: PhantomData<E2>,
}

impl<S, Request, E2> Batch<S, Request, E2>
where
    S: Service<BatchControl<Request>>,
    S::Error: Into<E2> + Clone,
    E2: Send + 'static,
    crate::error::Closed: Into<E2>,
    // crate::error::Closed: Into<<Self as Service<Request>>::Error> + Send + Sync + 'static,
    // crate::error::ServiceError: Into<<Self as Service<Request>>::Error> + Send + Sync + 'static,
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
    pub fn new(service: S, max_items: usize, max_latency: std::time::Duration) -> Self
    where
        S: Send + 'static,
        S::Future: Send,
        S::Error: Send + Sync + Clone,
        Request: Send + 'static,
    {
        // XXX(hdevalence): is this bound good
        let (tx, rx) = mpsc::channel(1);
        let (handle, worker) = Worker::new(service, rx, max_items, max_latency);
        tokio::spawn(worker.run());
        Batch {
            tx,
            handle,
            _e: PhantomData,
        }
    }

    fn get_worker_error(&self) -> E2 {
        self.handle.get_error_on_closed()
    }
}

impl<S, Request, E2> Service<Request> for Batch<S, Request, E2>
where
    S: Service<BatchControl<Request>>,
    crate::error::Closed: Into<E2>,
    S::Error: Into<E2> + Clone,
    E2: Send + 'static,
{
    type Response = S::Response;
    type Error = E2;
    type Future = ResponseFuture<S, E2, Request>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // If the inner service has errored, then we error here.
        if ready!(self.tx.poll_ready(cx)).is_err() {
            Poll::Ready(Err(self.get_worker_error()))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, request: Request) -> Self::Future {
        // TODO:
        // ideally we'd poll_ready again here so we don't allocate the oneshot
        // if the try_send is about to fail, but sadly we can't call poll_ready
        // outside of task context.
        let (tx, rx) = oneshot::channel();

        // get the current Span so that we can explicitly propagate it to the worker
        // if we didn't do this, events on the worker related to this span wouldn't be counted
        // towards that span since the worker would have no way of entering it.
        let span = tracing::Span::current();
        tracing::trace!(parent: &span, "sending request to batch worker");
        match self.tx.try_send(Message { request, span, tx }) {
            Err(mpsc::error::TrySendError::Closed(_)) => {
                ResponseFuture::failed(self.get_worker_error())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                // When `mpsc::Sender::poll_ready` returns `Ready`, a slot
                // in the channel is reserved for the handle. Other `Sender`
                // handles may not send a message using that slot. This
                // guarantees capacity for `request`.
                //
                // Given this, the only way to hit this code path is if
                // `poll_ready` has not been called & `Ready` returned.
                panic!("buffer full; poll_ready must be called first");
            }
            Ok(_) => ResponseFuture::new(rx),
        }
    }
}

impl<S, Request> Clone for Batch<S, Request>
where
    S: Service<BatchControl<Request>>,
{
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            handle: self.handle.clone(),
            _e: PhantomData,
        }
    }
}
