//! Core Zcash data structures.
//!
//! This crate provides definitions of core data structures for Zcash, such as
//! blocks, transactions, addresses, etc.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_chain")]
// Required by bitvec! macro
#![recursion_limit = "256"]

#[macro_use]
extern crate bitflags;

#[macro_use]
extern crate serde;

#[macro_use]
extern crate tracing;

pub mod amount;
pub mod block;
pub mod block_info;
pub mod chain_sync_status;
pub mod chain_tip;
pub mod common;
pub mod diagnostic;
pub mod error;
pub mod fmt;
pub mod history_tree;
pub mod orchard;
pub mod parallel;
pub mod parameters;
pub mod primitives;
pub mod sapling;
pub mod serialization;
pub mod shutdown;
pub mod sprout;
pub mod subtree;
pub mod tachyon;
pub mod transaction;
pub mod transparent;
pub mod value_balance;
pub mod work;

pub use error::Error;

#[cfg(any(test, feature = "proptest-impl"))]
pub use block::LedgerState;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

/// Error type alias to make working with generic errors easier.
///
/// Note: the 'static lifetime bound means that the *type* cannot have any
/// non-'static lifetimes, (e.g., when a type contains a borrow and is
/// parameterized by 'a), *not* that the object itself has 'static lifetime.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
