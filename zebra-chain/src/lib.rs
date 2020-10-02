//! Core Zcash data structures. 🦓
//!
//! This crate provides definitions of core datastructures for Zcash, such as
//! blocks, transactions, addresses, etc.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_chain")]
// #![deny(missing_docs)]
#![allow(clippy::try_err)]

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

#[derive(Debug, Clone, Copy)]
#[cfg(any(test, feature = "proptest-impl"))]
pub struct LedgerState {
    pub tip_height: block::Height,
    pub is_coinbase: bool,
    pub network: parameters::Network,
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
