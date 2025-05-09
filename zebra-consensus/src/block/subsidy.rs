//! Validate coinbase transaction rewards as described in [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

/// Funding Streams functions apply for blocks at and after Canopy.
pub mod funding_streams;
