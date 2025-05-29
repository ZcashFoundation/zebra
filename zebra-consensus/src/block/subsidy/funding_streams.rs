//! Funding Streams calculations. - [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use std::collections::HashMap;

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{subsidy::*, Network, NetworkUpgrade::*},
    transaction::Transaction,
    transparent::{self, Script},
};

#[cfg(test)]
mod tests;

/// Returns the `fs.Value(height)` for each stream receiver
/// as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn funding_stream_values(
    height: Height,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<HashMap<FundingStreamReceiver, Amount<NonNegative>>, Error> {
    let canopy_height = Canopy.activation_height(network).unwrap();
    let mut results = HashMap::new();

    if height >= canopy_height {
        let funding_streams = network.funding_streams(height);
        if funding_streams.height_range().contains(&height) {
            for (&receiver, recipient) in funding_streams.recipients() {
                // - Spec equation: `fs.value = floor(block_subsidy(height)*(fs.numerator/fs.denominator))`:
                //   https://zips.z.cash/protocol/protocol.pdf#subsidies
                // - In Rust, "integer division rounds towards zero":
                //   https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators
                //   This is the same as `floor()`, because these numbers are all positive.
                let amount_value = ((expected_block_subsidy * recipient.numerator())?
                    / FUNDING_STREAM_RECEIVER_DENOMINATOR)?;

                results.insert(receiver, amount_value);
            }
        }

        // It's okay to skip this check before the Canopy activation height because Canopy must be active when NU6.1 is active.
        if let Some((receiver, amount)) =
            network.lockbox_disbursement(height, expected_block_subsidy)
        {
            results.insert(receiver, amount);
        }
    }

    Ok(results)
}

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

    let index = 1u32
        .checked_add(funding_stream_address_period(height, network))
        .expect("no overflow should happen in this sum")
        .checked_sub(funding_stream_address_period(
            funding_streams.height_range().start,
            network,
        ))
        .expect("no overflow should happen in this sub") as usize;

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
    // TODO: Move this logic and the exception for the one-time lockbox to a method on `FundingStreamRecipient` or otherwise refactor
    if receiver == FundingStreamReceiver::CcfmKho {
        return Some(network.lockbox_disbursement_address());
    }

    let index = funding_stream_address_index(height, network, receiver)?;
    let funding_streams = network.funding_streams(height);
    funding_streams.recipient(receiver)?.addresses().get(index)
}

/// Given a funding stream P2SH address, create a script and check if it is the same
/// as the given lock_script as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub fn check_script_form(lock_script: &Script, address: &transparent::Address) -> bool {
    assert!(
        address.is_script_hash(),
        "incorrect funding stream address constant: {address} \
         Zcash only supports transparent 'pay to script hash' (P2SH) addresses",
    );

    // Verify a Bitcoin P2SH single or multisig address.
    let standard_script_hash = new_coinbase_script(address);

    lock_script == &standard_script_hash
}

/// Returns a new funding stream coinbase output lock script, which pays to the P2SH `address`.
pub fn new_coinbase_script(address: &transparent::Address) -> Script {
    assert!(
        address.is_script_hash(),
        "incorrect coinbase script address: {address} \
         Funding streams only support transparent 'pay to script hash' (P2SH) addresses",
    );

    // > The “prescribed way” to pay a transparent P2SH address is to use a standard P2SH script
    // > of the form OP_HASH160 fs.RedeemScriptHash(height) OP_EQUAL as the scriptPubKey.
    //
    // [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
    address.create_script_from_address()
}

/// Returns a list of outputs in `transaction`, which have a script address equal to `address`.
pub fn filter_outputs_by_address(
    transaction: &Transaction,
    address: &transparent::Address,
) -> Vec<transparent::Output> {
    transaction
        .outputs()
        .iter()
        .filter(|o| check_script_form(&o.lock_script, address))
        .cloned()
        .collect()
}
