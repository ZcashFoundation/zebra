//! Core Zcash data structures.
//!
//! This crate provides definitions of core data structures for Zcash, such as
//! blocks, transactions, addresses, etc.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_chain")]
// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![forbid(unsafe_code)]
// Required by bitvec! macro
#![recursion_limit = "256"]

#[macro_use]
extern crate serde;
#[macro_use]
extern crate serde_big_array;

#[macro_use]
extern crate bitflags;

pub mod amount;
pub mod block;
pub mod chain_tip;
pub mod fmt;
pub mod history_tree;
pub mod orchard;
pub mod parameters;
pub mod primitives;
pub mod sapling;
pub mod serialization;
pub mod shutdown;
pub mod sprout;
pub mod transaction;
pub mod transparent;
pub mod value_balance;
pub mod work;

#[cfg(any(test, feature = "proptest-impl"))]
pub use block::LedgerState;
