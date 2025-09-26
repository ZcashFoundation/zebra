//! Tower service layer for batch processing.

use std::{fmt, marker::PhantomData};

use tower::layer::Layer;
use tower::Service;

use crate::RequestWeight;

use super::{service::Batch, BatchControl};

/// Adds a layer performing batch processing of requests.
///
/// The default Tokio executor is used to run the given service,
/// which means that this layer can only be used on the Tokio runtime.
///
/// See the module documentation for more details.
pub struct BatchLayer<Request> {
    max_items_weight_in_batch: usize,
    max_batches: Option<usize>,
    max_latency: std::time::Duration,

    // TODO: is the variance correct here?
    // https://doc.rust-lang.org/1.33.0/nomicon/subtyping.html#variance
    // https://doc.rust-lang.org/nomicon/phantom-data.html#table-of-phantomdata-patterns
    _handles_requests: PhantomData<fn(Request)>,
}

impl<Request> BatchLayer<Request> {
    /// Creates a new `BatchLayer`.
    ///
    /// The wrapper is responsible for telling the inner service when to flush a
    /// batch of requests. See [`Batch::new()`] for details.
    pub fn new(
        max_items_weight_in_batch: usize,
        max_batches: impl Into<Option<usize>>,
        max_latency: std::time::Duration,
    ) -> Self {
        BatchLayer {
            max_items_weight_in_batch,
            max_batches: max_batches.into(),
            max_latency,
            _handles_requests: PhantomData,
        }
    }
}

impl<S, Request: RequestWeight> Layer<S> for BatchLayer<Request>
where
    S: Service<BatchControl<Request>> + Send + 'static,
    S::Future: Send,
    S::Response: Send,
    S::Error: Into<crate::BoxError> + Send + Sync,
    Request: Send + 'static,
{
    type Service = Batch<S, Request>;

    fn layer(&self, service: S) -> Self::Service {
        Batch::new(
            service,
            self.max_items_weight_in_batch,
            self.max_batches,
            self.max_latency,
        )
    }
}

impl<Request> fmt::Debug for BatchLayer<Request> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("BatchLayer")
            .field("max_items_weight_in_batch", &self.max_items_weight_in_batch)
            .field("max_batches", &self.max_batches)
            .field("max_latency", &self.max_latency)
            .finish()
    }
}
