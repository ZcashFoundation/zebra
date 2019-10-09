//! Networking code for Zebra. 🦓

#![deny(missing_docs)]

#[macro_use]
extern crate serde;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate tracing;
#[macro_use]
extern crate bitflags;

/// Type alias to make working with tower traits easier.
///
/// Note: the 'static lifetime bound means that the *type* cannot have any
/// non-'static lifetimes, (e.g., when a type contains a borrow and is
/// parameterized by 'a), *not* that the object itself has 'static lifetime.
pub(crate) type BoxedStdError = Box<dyn std::error::Error + Send + Sync + 'static>;

mod network;
pub use network::Network;

mod config;
pub use config::Config;

pub mod protocol;

// XXX revisit privacy once we finish encapsulation.
pub mod constants;
pub mod meta_addr;
pub mod peer;
pub mod peer_set;
pub mod timestamp_collector;
