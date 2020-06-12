pub mod error;
pub mod future;
mod layer;
mod message;
mod service;
mod worker;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub enum BatchControl<R> {
    Item(R),
    Flush,
}

impl<R> From<R> for BatchControl<R> {
    fn from(req: R) -> BatchControl<R> {
        BatchControl::Item(req)
    }
}

pub use self::layer::BatchLayer;
pub use self::service::Batch;
