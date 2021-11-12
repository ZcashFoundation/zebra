//! Validate coinbase transaction rewards as described in [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

/// Founders' Reward functions apply for blocks before Canopy.
pub mod founders_reward;
/// Funding Streams functions apply for blocks at and after Canopy.
pub mod funding_streams;
/// General subsidy functions apply for blocks after slow-start mining.
pub mod general;
