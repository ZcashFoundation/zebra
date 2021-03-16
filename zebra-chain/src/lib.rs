//! Core Zcash data structures. 🦓
//!
//! This crate provides definitions of core data structures for Zcash, such as
//! blocks, transactions, addresses, etc.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_chain")]
#![allow(clippy::try_err)]
// Use the old lint name to build without warnings on stable until 1.51 is stable
#![allow(clippy::unknown_clippy_lints)]
// The actual lints we want to disable
#![allow(clippy::unnecessary_wraps)]
// Disable some broken or unwanted clippy nightly lints
// Build without warnings on nightly 2021-01-17 and later and stable 1.51 and later
#![allow(unknown_lints)]
// Disable old lint warnings on nightly until 1.51 is stable
#![allow(renamed_and_removed_lints)]

#[macro_use]
extern crate serde;

pub mod amount;
pub mod block;
pub mod fmt;
pub mod orchard;
pub mod parameters;
pub mod primitives;
pub mod sapling;
pub mod serialization;
pub mod shutdown;
pub mod sprout;
pub mod transaction;
pub mod transparent;
pub mod work;

#[cfg(any(test, feature = "proptest-impl"))]
pub use block::LedgerState;
