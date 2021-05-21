//! Holds components of a Zebra node.
//!
//! Some, but not all, of these components are structured as Abscissa components,
//! while the others are just ordinary structures. This is because Abscissa's
//! component and dependency injection models are designed to work together, but
//! don't fit the async context well.

mod inbound;
pub mod metrics;
mod sync;
pub mod tokio;
pub mod tracing;

pub use inbound::{Inbound, MAX_INBOUND_DOWNLOAD_CONCURRENCY};
pub use sync::ChainSync;
