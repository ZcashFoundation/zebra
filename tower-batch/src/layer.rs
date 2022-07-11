//! Tower service layer for batch processing.

use std::{fmt, marker::PhantomData};

use tower::layer::Layer;
use tower::Service;

use super::{service::Batch, BatchControl};

/// Adds a layer performing batch processing of requests.
///
/// The default Tokio executor is used to run the given service,
/// which means that this layer can only be used on the Tokio runtime.
///
/// See the module documentation for more details.
pub struct BatchLayer<Request> {
    max_items_in_batch: usize,
    max_batches_in_queue: Option<usize>,
    max_latency: std::time::Duration,

    _p: PhantomData<fn(Request)>,
}

impl<Request> BatchLayer<Request> {
    /// Creates a new `BatchLayer`.
    ///
    /// The wrapper is responsible for telling the inner service when to flush a
    /// batch of requests. These parameters control this policy:
    ///
    /// * `max_items_in_batch` gives the maximum number of items per batch.
    /// * `max_batches_in_queue` is an upper bound on the number of batches in the queue.
    ///   If this is `None`, the `Batch` uses the current number of [`rayon`] threads.
    ///   The final value is automatically limited to [`MAX_BATCHES_IN_QUEUE`].
    /// * `max_latency` gives the maximum latency for a batch item to start verifying.
    pub fn new(
        max_items_in_batch: usize,
        max_batches_in_queue: impl Into<Option<usize>>,
        max_latency: std::time::Duration,
    ) -> Self {
        BatchLayer {
            max_items_in_batch,
            max_batches_in_queue: max_batches_in_queue.into(),
            max_latency,
            _p: PhantomData,
        }
    }
}

impl<S, Request> Layer<S> for BatchLayer<Request>
where
    S: Service<BatchControl<Request>> + Send + 'static,
    S::Future: Send,
    S::Error: Into<crate::BoxError> + Send + Sync,
    Request: Send + 'static,
{
    type Service = Batch<S, Request>;

    fn layer(&self, service: S) -> Self::Service {
        Batch::new(
            service,
            self.max_items_in_batch,
            self.max_batches_in_queue,
            self.max_latency,
        )
    }
}

impl<Request> fmt::Debug for BatchLayer<Request> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("BufferLayer")
            .field("max_items_in_batch", &self.max_items_in_batch)
            .field("max_batches_in_queue", &self.max_batches_in_queue)
            .field("max_latency", &self.max_latency)
            .finish()
    }
}
