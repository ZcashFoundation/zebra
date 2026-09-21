//! Funding Streams calculations [§7.10]
//!
//! [§7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams

use zebra_chain::{
    block::Height,
    parameters::{subsidy::*, Network},
    transparent::{self},
};

#[cfg(test)]
mod tests;

/// Returns the position in the address slice for each funding stream
/// as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
fn funding_stream_address_index(
    height: Height,
    network: &Network,
    receiver: FundingStreamReceiver,
) -> Option<usize> {
    if receiver == FundingStreamReceiver::Deferred {
        return None;
    }

    let funding_streams = network.funding_streams(height)?;
    let num_addresses = funding_streams.recipient(receiver)?.addresses().len();

    // The two periods are only meaningful relative to each other, so the subtraction is done in
    // signed arithmetic: see `funding_stream_address_period()`.
    let index = usize::try_from(
        1 + funding_stream_address_period(height, network)
            - funding_stream_address_period(funding_streams.height_range().start, network),
    )
    .ok()?;

    assert!(index > 0 && index <= num_addresses);
    // spec formula will output an index starting at 1 but
    // Zebra indices for addresses start at zero, return converted.
    Some(index - 1)
}

/// Return the address corresponding to given height, network and funding stream receiver.
///
/// This function only returns transparent addresses, because the current Zcash funding streams
/// only use transparent addresses,
pub fn funding_stream_address(
    height: Height,
    network: &Network,
    receiver: FundingStreamReceiver,
) -> Option<&transparent::Address> {
    let index = funding_stream_address_index(height, network, receiver)?;
    let funding_streams = network.funding_streams(height)?;
    funding_streams.recipient(receiver)?.addresses().get(index)
}
