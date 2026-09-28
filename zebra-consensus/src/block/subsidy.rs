//! Funding Streams calculations [§7.10]
//!
//! [§7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams

use zebra_chain::{
    block::Height,
    parameters::{subsidy::FundingStreamReceiver, Network},
    transparent,
};

/// Return the address corresponding to given height, network and funding stream receiver.
///
/// This function only returns transparent addresses, because the current Zcash funding streams
/// only use transparent addresses.
///
/// The calculation lives in [`zebra_chain::parameters::subsidy::funding_stream_address`]. This
/// wrapper keeps the function at its original path in this crate: `cargo-semver-checks` can't
/// see a re-export from another crate, and reports a plain `pub use` as a removed function.
pub fn funding_stream_address(
    height: Height,
    network: &Network,
    receiver: FundingStreamReceiver,
) -> Option<&transparent::Address> {
    zebra_chain::parameters::subsidy::funding_stream_address(height, network, receiver)
}

#[cfg(test)]
mod tests;
