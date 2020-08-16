//! Core Zcash data structures. 🦓
//!
//! This crate provides definitions of core datastructures for Zcash, such as
//! blocks, transactions, addresses, etc.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_chain")]
#![allow(clippy::try_err)]
#![deny(missing_docs)]

#[macro_use]
extern crate serde;

pub mod amount;
pub mod block;
pub mod parameters;
pub mod primitives;
pub mod sapling;
pub mod serialization;
pub mod sprout;
pub mod transaction;
pub mod transparent;
pub mod work;
