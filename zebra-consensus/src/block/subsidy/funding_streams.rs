//! Funding Streams calculations. - [§7.8][7.8]
//!
//! [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies

use zebra_chain::{
    amount::{Amount, Error, NonNegative},
    block::Height,
    parameters::{Network, NetworkUpgrade::*},
    serialization::ZcashSerialize,
    transaction::Transaction,
    transparent::{Address, Output, Script},
};

use crate::{block::subsidy::general::block_subsidy, parameters::subsidy::*};

use std::{collections::HashMap, str::FromStr};

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
pub fn funding_stream_address(
    height: Height,
    network: Network,
    receiver: FundingStreamReceiver,
) -> Address {
    let index = funding_stream_address_index(height, network);
    let address = &FUNDING_STREAM_ADDRESSES
        .get(&network)
        .expect("there is always another hash map as value for a given valid network")
        .get(&receiver)
        .expect("in the inner hash map there is always a vector of strings with addresses")[index];
    Address::from_str(address).expect("Address should deserialize")
}

/// Given a funding stream address, create a script and check if it is the same
/// as the given lock_script as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub fn check_script_form(lock_script: &Script, address: Address) -> bool {
    let mut address_hash = address
        .zcash_serialize_to_vec()
        .expect("we should get address bytes here");

    address_hash = address_hash[2..22].to_vec();
    address_hash.insert(0, OpCode::Push20Bytes as u8);
    address_hash.insert(0, OpCode::Hash160 as u8);
    address_hash.insert(address_hash.len(), OpCode::Equal as u8);
    if lock_script.as_raw_bytes().len() == address_hash.len()
        && *lock_script == Script::new(&address_hash)
    {
        return true;
    }
    false
}

/// Returns a list of outputs in `Transaction`, which have a script address equal to `Address`.
pub fn filter_outputs_by_address(transaction: &Transaction, address: Address) -> Vec<Output> {
    transaction
        .outputs()
        .iter()
        .filter(|o| check_script_form(&o.lock_script, address))
        .cloned()
        .collect()
}

/// Script opcodes needed to compare the `lock_script` with the funding stream reward address.
pub enum OpCode {
    Equal = 0x87,
    Hash160 = 0xa9,
    Push20Bytes = 0x14,
}
