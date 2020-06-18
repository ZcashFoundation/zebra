use super::{service::Batch, BatchControl};
use std::{fmt, marker::PhantomData};
use tower::layer::Layer;
use tower::Service;

/// Adds a layer performing batch processing of requests.
///
/// The default Tokio executor is used to run the given service,
/// which means that this layer can only be used on the Tokio runtime.
///
/// See the module documentation for more details.
pub struct BatchLayer<Request, E> {
    max_items: usize,
    max_latency: std::time::Duration,
    _p: PhantomData<fn(Request)>,
    _e: PhantomData<E>,
}

impl<Request, E> BatchLayer<Request, E> {
    /// Creates a new `BatchLayer`.
    ///
    /// The wrapper is responsible for telling the inner service when to flush a
    /// batch of requests.  Two parameters control this policy:
    ///
    /// * `max_items` gives the maximum number of items per batch.
    /// * `max_latency` gives the maximum latency for a batch item.
    pub fn new(max_items: usize, max_latency: std::time::Duration) -> Self {
        BatchLayer {
            max_items,
            max_latency,
            _p: PhantomData,
            _e: PhantomData,
        }
    }
}

impl<S, Request, E> Layer<S> for BatchLayer<Request, E>
where
    S: Service<BatchControl<Request>> + Send + 'static,
    S::Future: Send,
    S::Error: Clone + Into<E> + Send + Sync,
    Request: Send + 'static,
    E: Send + 'static,
    crate::error::Closed: Into<E>,
{
    type Service = Batch<S, Request, E>;

    fn layer(&self, service: S) -> Self::Service {
        Batch::new(service, self.max_items, self.max_latency)
    }
}

impl<Request, E> fmt::Debug for BatchLayer<Request, E> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("BufferLayer")
            .field("max_items", &self.max_items)
            .field("max_latency", &self.max_latency)
            .finish()
    }
}
