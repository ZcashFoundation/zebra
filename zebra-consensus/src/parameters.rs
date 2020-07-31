//! The consensus parameters for each Zcash network.
//!
//! Some consensus parameters change based on network upgrades. Each network
//! upgrade happens at a particular block height. Some parameters have a value
//! (or function) before the upgrade height, at the upgrade height, and after
//! the upgrade height. (For example, the value of the reserved field in the
//! block header during the Heartwood upgrade.)
//!
//! Typically, consensus parameters are accessed via a function that takes a
//! `Network` and `BlockHeight`.

pub mod genesis;
pub mod minimum_difficulty;
pub mod network_upgrade;

pub use genesis::*;
pub use minimum_difficulty::*;
pub use network_upgrade::*;

#[cfg(test)]
mod tests;
