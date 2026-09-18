//! Funding Streams calculations [§7.10]
//!
//! [§7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams

pub use zebra_chain::parameters::subsidy::funding_stream_address;

#[cfg(test)]
mod tests;
