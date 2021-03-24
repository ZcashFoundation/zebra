//! Validate coinbase transaction rewards as described in [§7.7][7.7]
//!
//! [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies

/// Founders' Reward functions apply for blocks before Canopy.
pub mod founders_reward;
/// General subsidy functions apply for blocks after slow-start mining.
pub mod general;
