//! The interfaces of some Zebra node services.

pub use zebra_chain::parameters::checkpoint::constants;
pub mod mempool;

#[cfg(any(test, feature = "rpc-client"))]
pub mod rpc_client;

/// Error type alias to make working with tower traits easier.
///
/// Note: the 'static lifetime bound means that the *type* cannot have any
/// non-'static lifetimes, (e.g., when a type contains a borrow and is
/// parameterized by 'a), *not* that the object itself has 'static lifetime.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
