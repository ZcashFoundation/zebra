//! Core Zcash data structures. 🦓
//!
//! This crate provides definitions of core datastructures for Zcash, such as
//! blocks, transactions, addresses, etc.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_chain")]
// #![deny(missing_docs)]
#![allow(clippy::try_err)]
// Disable some broken or unwanted clippy nightly lints
#![allow(clippy::unknown_clippy_lints)]
#![allow(clippy::from_iter_instead_of_collect)]
#![allow(clippy::unnecessary_wraps)]

#[macro_use]
extern crate serde;

pub mod amount;
pub mod block;
pub mod fmt;
pub mod parameters;
pub mod primitives;
pub mod sapling;
pub mod serialization;
pub mod sprout;
pub mod transaction;
pub mod transparent;
pub mod work;

#[derive(Debug, Clone, Copy)]
#[cfg(any(test, feature = "proptest-impl"))]
#[non_exhaustive]
/// The configuration data for proptest when generating arbitrary chains
pub struct LedgerState {
    /// The tip height of the block or start of the chain
    pub tip_height: block::Height,
    is_coinbase: bool,
    /// The network to generate fake blocks for
    pub network: parameters::Network,
}

#[cfg(any(test, feature = "proptest-impl"))]
impl LedgerState {
    /// Construct a new ledger state for generating arbitrary chains via proptest
    pub fn new(tip_height: block::Height, network: parameters::Network) -> Self {
        Self {
            tip_height,
            is_coinbase: true,
            network,
        }
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl Default for LedgerState {
    fn default() -> Self {
        let network = parameters::Network::Mainnet;
        let tip_height = parameters::NetworkUpgrade::Sapling
            .activation_height(network)
            .unwrap();

        Self {
            tip_height,
            is_coinbase: true,
            network,
        }
    }
}
