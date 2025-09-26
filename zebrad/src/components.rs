//! Holds components of a Zebra node.
//!
//! Some, but not all, of these components are structured as Abscissa components,
//! while the others are just ordinary structures. This is because Abscissa's
//! component and dependency injection models are designed to work together, but
//! don't fit the async context well.

pub mod health;
pub mod inbound;
#[allow(missing_docs)]
pub mod mempool;
pub mod metrics;
#[allow(missing_docs)]
pub mod sync;
#[allow(missing_docs)]
pub mod tokio;
#[allow(missing_docs)]
pub mod tracing;

#[cfg(feature = "internal-miner")]
pub mod miner;

pub use inbound::Inbound;
pub use sync::ChainSync;
