//! Support functions for the `get_block_template()` RPC.

use std::iter;

use zebra_chain::{
    amount::{self, Amount, NegativeOrZero, NonNegative},
    block::{
        merkle::{self, AuthDataRoot},
        Height,
    },
    parameters::Network,
    transaction::{Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::{funding_stream_address, funding_stream_values, miner_subsidy};

use crate::methods::get_block_template_rpcs::types::transaction::TransactionTemplate;

// - Coinbase transaction functions

/// Returns the total miner fee for `mempool_txs`.
pub fn miner_fee(mempool_txs: &[VerifiedUnminedTx]) -> Amount<NonNegative> {
    let miner_fee: amount::Result<Amount<NonNegative>> =
        mempool_txs.iter().map(|tx| tx.miner_fee).sum();

    miner_fee.expect(
        "invalid selected transactions: \
         fees in a valid block can not be more than MAX_MONEY",
    )
}

/// Returns the standard funding stream and miner reward transparent output scripts
/// for `network`, `height` and `miner_fee`.
///
/// Only works for post-Canopy heights.
pub fn standard_coinbase_outputs(
    network: Network,
    height: Height,
    miner_address: transparent::Address,
    miner_fee: Amount<NonNegative>,
) -> Vec<(Amount<NonNegative>, transparent::Script)> {
    let funding_streams = funding_stream_values(height, network)
        .expect("funding stream value calculations are valid for reasonable chain heights");

    let mut funding_streams: Vec<(Amount<NonNegative>, transparent::Address)> = funding_streams
        .iter()
        .map(|(receiver, amount)| (*amount, funding_stream_address(height, network, *receiver)))
        .collect();
    // The HashMap returns funding streams in an arbitrary order,
    // but Zebra's snapshot tests expect the same order every time.
    funding_streams.sort_by_key(|(amount, _address)| *amount);

    let miner_reward = miner_subsidy(height, network)
        .expect("reward calculations are valid for reasonable chain heights")
        + miner_fee;
    let miner_reward =
        miner_reward.expect("reward calculations are valid for reasonable chain heights");

    let mut coinbase_outputs = funding_streams;
    coinbase_outputs.push((miner_reward, miner_address));

    coinbase_outputs
        .iter()
        .map(|(amount, address)| (*amount, address.create_script_from_address()))
        .collect()
}

/// Returns a fake coinbase transaction that can be used during transaction selection.
///
/// This avoids a data dependency loop involving the selected transactions, the miner fee,
/// and the coinbase transaction.
///
/// This transaction's serialized size and sigops must be at least as large as the real coinbase
/// transaction with the correct height and fee.
pub fn fake_coinbase_transaction(
    network: Network,
    block_height: Height,
    miner_address: transparent::Address,
) -> TransactionTemplate<NegativeOrZero> {
    // Block heights are encoded as variable-length (script) and `u32` (lock time, expiry height).
    // They can also change the `u32` consensus branch id.
    // We use the template height here, which has the correct byte length.
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    // https://github.com/zcash/zips/blob/main/zip-0203.rst#changes-for-nu5
    //
    // Transparent amounts are encoded as `i64`,
    // so one zat has the same size as the real amount:
    // https://developer.bitcoin.org/reference/transactions.html#txout-a-transaction-output
    let miner_fee = 1.try_into().expect("amount is valid and non-negative");

    let outputs = standard_coinbase_outputs(network, block_height, miner_address, miner_fee);
    let coinbase_tx = Transaction::new_v5_coinbase(network, block_height, outputs).into();

    TransactionTemplate::from_coinbase(&coinbase_tx, miner_fee)
}

// - Transaction roots functions

/// Returns the transaction effecting and authorizing roots
/// for `coinbase_tx` and `mempool_txs`, which are used in the block header.
//
// TODO: should this be spawned into a cryptographic operations pool?
//       (it would only matter if there were a lot of small transactions in a block)
pub fn calculate_transaction_roots(
    coinbase_tx: &UnminedTx,
    mempool_txs: &[VerifiedUnminedTx],
) -> (merkle::Root, AuthDataRoot) {
    let block_transactions =
        || iter::once(coinbase_tx).chain(mempool_txs.iter().map(|tx| &tx.transaction));

    let merkle_root = block_transactions().cloned().collect();
    let auth_data_root = block_transactions().cloned().collect();

    (merkle_root, auth_data_root)
}
