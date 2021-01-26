//! Networking code for Zebra. 🦓
//!
//! The Zcash network protocol is inherited from Bitcoin, which uses a
//! stateful network protocol in which messages can arrive in any
//! order (even before a handshake is complete!), and the same message
//! may be a request or a response depending on context.
//!
//! This crate translates the legacy Bitcoin/Zcash network protocol
//! into a stateless, request-response oriented protocol defined by
//! the [`Request`] and [`Response`] enums, and completely
//! encapsulates all peer handling code behind a single
//! [`tower::Service`] representing "the network" which load-balances
//! outbound [`Request`]s over available peers.
//!
//! Unlike the underlying legacy network protocol the provided
//! [`tower::Service`] guarantees that each `Request` future will resolve to
//! the correct `Response`, not a potentially random unrelated `Response`.
//!
//! Each peer connection is a distinct task, which interprets incoming
//! Bitcoin/Zcash messages either as [`Response`]s to pending
//! [`Request`]s, or as an inbound [`Request`]s to an internal
//! [`tower::Service`] representing "this node".  All connection state
//! is isolated to the peer in question, so as a side effect the
//! design is structurally immune to the recent `ping` attack.
//!
//! Because [`tower::Service`]s provide backpressure information, we
//! can dynamically manage the size of the connection pool according
//! to inbound and outbound demand.  The inbound service can shed load
//! when it is not ready for requests, causing those peer connections
//! to close, and the outbound service can connect to additional peers
//! when it is overloaded.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_network")]
#![deny(missing_docs)]
#![allow(clippy::try_err)]
// Tracing causes false positives on this lint:
// https://github.com/tokio-rs/tracing/issues/553
#![allow(clippy::cognitive_complexity)]
// Disable some broken or unwanted clippy nightly lints
// Build without warnings on nightly 2021-01-17 and later and stable 1.51 and later
#![allow(unknown_lints)]
// Disable old lint warnings on nightly until 1.51 is stable
#![allow(renamed_and_removed_lints)]
// Use the old lint name to build without warnings on stable until 1.51 is stable
#![allow(clippy::unknown_clippy_lints)]
// The actual lints we want to disable
#![allow(clippy::unnecessary_wraps)]

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
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

mod address_book;
mod config;
pub mod constants;
mod isolated;
mod meta_addr;
mod peer;
mod peer_set;
mod policies;
mod protocol;
mod timestamp_collector;

pub use crate::{
    address_book::AddressBook,
    config::Config,
    isolated::connect_isolated,
    peer_set::init,
    policies::{RetryErrors, RetryLimit},
    protocol::internal::{Request, Response},
};

/// Types used in the definition of [`Request`] and [`Response`] messages.
pub mod types {
    pub use crate::{meta_addr::MetaAddr, protocol::types::PeerServices};
}

use std::sync::atomic::AtomicBool;
/// A flag to indicate if zebrad is shutting down.
pub static IS_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
