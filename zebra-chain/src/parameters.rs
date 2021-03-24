//! Consensus parameters for each Zcash network.
//!
//! This module contains the consensus parameters which are required for
//! parsing.
//!
//! Some consensus parameters change based on network upgrades. Each network
//! upgrade happens at a particular block height. Some parameters have a value
//! (or function) before the upgrade height, at the upgrade height, and after
//! the upgrade height. (For example, the value of the reserved field in the
//! block header during the Heartwood upgrade.)
//!
//! Typically, consensus parameters are accessed via a function that takes a
//! `Network` and `block::Height`.

mod genesis;
mod network;
mod network_upgrade;
mod transaction;

pub use genesis::*;
pub use network::Network;
pub use network_upgrade::*;
pub use transaction::*;

#[cfg(test)]
mod tests;
