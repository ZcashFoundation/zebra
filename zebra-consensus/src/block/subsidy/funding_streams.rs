//! Funding Streams calculations. - [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

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

    let funding_streams = network.funding_streams(height);
    let num_addresses = funding_streams.recipient(receiver)?.addresses().len();
    let height_range_pos = funding_streams
        .height_ranges()
        .iter()
        .position(|range| range.contains(&height))?;

    let (past_height_ranges, [current_height_range, ..]) =
        funding_streams.height_ranges().split_at(height_range_pos)
    else {
        unreachable!("index found with position() must exist in the slice");
    };

    // Start the index of each height range after the last index of the previous range.
    // This logic will not re-use any addresses across height ranges.
    let index = past_height_ranges
        .iter()
        .chain(std::iter::once(&(current_height_range.start..height)))
        .fold(1u32, |acc, height_range| {
            acc.checked_add(funding_stream_address_period(height_range.end, network))
                .expect("no overflow should happen in this sum")
                .checked_sub(funding_stream_address_period(height_range.start, network))
                .expect("no overflow should happen in this sub")
        }) as usize;

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
    let funding_streams = network.funding_streams(height);
    funding_streams.recipient(receiver)?.addresses().get(index)
}
