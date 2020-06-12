pub mod error;
pub mod future;
mod layer;
mod message;
mod service;
mod worker;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub use self::layer::BufferLayer;
pub use self::service::Buffer;
