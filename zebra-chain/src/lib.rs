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

#[derive(Default)]
#[cfg(any(test, feature = "proptest-impl"))]
pub struct LedgerState(pub Option<InProgressLedgerState>);

#[cfg(any(test, feature = "proptest-impl"))]
pub struct InProgressLedgerState {
    tip_height: block::Height,
    tip_hash: block::Hash,
    network: parameters::Network,
}

#[cfg(any(test, feature = "proptest-impl"))]
impl Default for InProgressLedgerState {
    fn default() -> Self {
        let network = parameters::Network::Mainnet;
        let tip_height = parameters::NetworkUpgrade::Sapling
            .activation_height(network)
            .unwrap();
        let tip_hash = block::Hash(Default::default());

        Self {
            tip_hash,
            tip_height,
            network,
        }
    }
}
