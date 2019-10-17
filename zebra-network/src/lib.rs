//! Networking code for Zebra. 🦓

#![deny(missing_docs)]

#[macro_use]
extern crate pin_project;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate tracing;
#[macro_use]
extern crate bitflags;

/// Type alias to make working with tower traits easier.
///
/// Note: the 'static lifetime bound means that the *type* cannot have any
/// non-'static lifetimes, (e.g., when a type contains a borrow and is
/// parameterized by 'a), *not* that the object itself has 'static lifetime.
pub type BoxedStdError = Box<dyn std::error::Error + Send + Sync + 'static>;

mod config;
mod constants;
mod meta_addr;
mod network;
mod peer;
mod peer_set;
mod protocol;
mod timestamp_collector;

pub use config::Config;
pub use meta_addr::MetaAddr;
pub use network::Network;
pub use peer_set::init;
pub use protocol::internal::{Request, Response};
pub use timestamp_collector::TimestampCollector;

/// This will be removed when we finish encapsulation
pub mod should_be_private {
    pub use crate::peer::PeerConnector;
}
