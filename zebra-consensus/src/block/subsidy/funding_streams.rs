//! Funding Streams calculations. - [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use std::{collections::HashMap, str::FromStr};

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
    transaction::Transaction,
    transparent::{self, Script},
};

use crate::{block::subsidy::general::block_subsidy, parameters::subsidy::*};

#[cfg(test)]
mod tests;

/// Returns the `fs.Value(height)` for each stream receiver
/// as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn funding_stream_values(
    height: Height,
    network: Network,
) -> Result<HashMap<FundingStreamReceiver, Amount<NonNegative>>, Error> {
    let canopy_height = Canopy.activation_height(network).unwrap();
    let mut results = HashMap::new();

    if height >= canopy_height {
        let range = FUNDING_STREAM_HEIGHT_RANGES.get(&network).unwrap();
        if range.contains(&height) {
            let block_subsidy = block_subsidy(height, network)?;
            for (&receiver, &numerator) in FUNDING_STREAM_RECEIVER_NUMERATORS.iter() {
                // - Spec equation: `fs.value = floor(block_subsidy(height)*(fs.numerator/fs.denominator))`:
                //   https://zips.z.cash/protocol/protocol.pdf#subsidies
                // - In Rust, "integer division rounds towards zero":
                //   https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators
                //   This is the same as `floor()`, because these numbers are all positive.
                let amount_value =
                    ((block_subsidy * numerator)? / FUNDING_STREAM_RECEIVER_DENOMINATOR)?;

                results.insert(receiver, amount_value);
            }
        }
    }
    Ok(results)
}

/// Returns the minimum height after the first halving
/// as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub fn height_for_first_halving(network: Network) -> Height {
    // First halving on Mainnet is at Canopy
    // while in Testnet is at block constant height of `1_116_000`
    // https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    match network {
        Network::Mainnet => Canopy
            .activation_height(network)
            .expect("canopy activation height should be available"),
        Network::Testnet => FIRST_HALVING_TESTNET,
    }
}

/// Returns the address change period
/// as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
fn funding_stream_address_period(height: Height, network: Network) -> u32 {
    // - Spec equation: `address_period = floor((height - height_for_halving(1) - post_blossom_halving_interval)/funding_stream_address_change_interval)`:
    //   https://zips.z.cash/protocol/protocol.pdf#fundingstreams
    // - In Rust, "integer division rounds towards zero":
    //   https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators
    //   This is the same as `floor()`, because these numbers are all positive.
    (height.0 + (POST_BLOSSOM_HALVING_INTERVAL.0) - (height_for_first_halving(network).0))
        / (FUNDING_STREAM_ADDRESS_CHANGE_INTERVAL.0)
}

/// Returns the position in the address slice for each funding stream
/// as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
fn funding_stream_address_index(height: Height, network: Network) -> usize {
    let num_addresses = match network {
        Network::Mainnet => FUNDING_STREAMS_NUM_ADDRESSES_MAINNET,
        Network::Testnet => FUNDING_STREAMS_NUM_ADDRESSES_TESTNET,
    };

    let index = 1u32
        .checked_add(funding_stream_address_period(height, network))
        .expect("no overflow should happen in this sum")
        .checked_sub(funding_stream_address_period(
            FUNDING_STREAM_HEIGHT_RANGES.get(&network).unwrap().start,
            network,
        ))
        .expect("no overflow should happen in this sub") as usize;

    assert!(index > 0 && index <= num_addresses);
    // spec formula will output an index starting at 1 but
    // Zebra indices for addresses start at zero, return converted.
    index - 1
}

/// Return the address corresponding to given height, network and funding stream receiver.
///
/// This function only returns transparent addresses, because the current Zcash funding streams
/// only use transparent addresses,
pub fn funding_stream_address(
    height: Height,
    network: Network,
    receiver: FundingStreamReceiver,
) -> transparent::Address {
    let index = funding_stream_address_index(height, network);
    let address = &FUNDING_STREAM_ADDRESSES
        .get(&network)
        .expect("there is always another hash map as value for a given valid network")
        .get(&receiver)
        .expect("in the inner hash map there is always a vector of strings with addresses")[index];
    transparent::Address::from_str(address).expect("address should deserialize")
}

/// Return a human-readable name and a specification URL for the funding stream `receiver`.
pub fn funding_stream_recipient_info(
    receiver: FundingStreamReceiver,
) -> (&'static str, &'static str) {
    let name = FUNDING_STREAM_NAMES
        .get(&receiver)
        .expect("all funding streams have a name");

    (name, FUNDING_STREAM_SPECIFICATION)
}

/// Given a funding stream P2SH address, create a script and check if it is the same
/// as the given lock_script as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub fn check_script_form(lock_script: &Script, address: transparent::Address) -> bool {
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
pub fn new_coinbase_script(address: transparent::Address) -> Script {
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
    address: transparent::Address,
) -> Vec<transparent::Output> {
    transaction
        .outputs()
        .iter()
        .filter(|o| check_script_form(&o.lock_script, address))
        .cloned()
        .collect()
}
