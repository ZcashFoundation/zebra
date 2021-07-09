//! Consensus rule checks for the finalized state.

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use itertools::Itertools;

use rocksdb::ColumnFamily;
use zebra_chain::{
    block::Block,
    transparent::{self, OutPoint},
};

use crate::{BoxError, OrderedUtxo};

use super::disk_format::IntoDisk;

/// Reject double-spends of transparent outputs:
/// - duplicate spends that are both in this block,
/// - spends of an output that hasn't been created yet,
///   (in linear chain and transaction order), and
/// - spends of an output that was spent by a previous block.
///
/// Returns the unique set of spends in `block_spends`,
/// including any that spend outputs created by this block.
///
/// "each output of a particular transaction
/// can only be used as an input once in the block chain.
/// Any subsequent reference is a forbidden double spend-
/// an attempt to spend the same satoshis twice."
///
/// https://developer.bitcoin.org/devguide/block_chain.html#introduction
///
/// "Any input within this block can spend an output which also appears in this block
/// (assuming the spend is otherwise valid).
/// However, the TXID corresponding to the output must be placed at some point
/// before the TXID corresponding to the input.
/// This ensures that any program parsing block chain transactions linearly
/// will encounter each output before it is used as an input."
///
/// https://developer.bitcoin.org/reference/block_chain.html#merkle-trees
pub fn transparent_double_spends(
    db: &rocksdb::DB,
    cf: &ColumnFamily,
    block: &Block,
    new_outputs: &HashMap<OutPoint, OrderedUtxo>,
) -> Result<HashSet<transparent::OutPoint>, BoxError> {
    let mut block_spends = HashSet::new();

    for (spend_tx_index_in_block, transaction) in block.transactions.iter().enumerate() {
        let spends = transaction.inputs().iter().filter_map(|input| match input {
            transparent::Input::PrevOut { outpoint, .. } => Some(outpoint),
            // Coinbase inputs represent new coins,
            // so there are no UTXOs to mark as spent.
            transparent::Input::Coinbase { .. } => None,
        });

        for spend in spends {
            // check for in-block duplicate spends
            if block_spends.contains(spend) {
                return Err("duplicate transparent OutPoint spends within block".into());
            }

            // check spends occur in chain order
            //
            // because we are in the finalized state, there is a single chain of ordered blocks,
            // so we just need to check spends within the same block, and the finalized UTXOs.

            if let Some(output) = new_outputs.get(spend) {
                // reject the spend if it uses an output from this block,
                // but the output was not created by an earlier transaction
                //
                // we know the spend is invalid, because transaction IDs are unique
                if output.tx_index_in_block >= spend_tx_index_in_block {
                    return Err("spend of transparent OutPoint created in the current or later transaction in block ".into());
                }
            } else {
                // reject the spend if its UTXO is not available in the state or the block
                if db.get_cf(cf, spend.as_bytes())?.is_none() {
                    return Err(
                        "no unspent output in previous block transaction or finalized state".into(),
                    );
                }
            }

            block_spends.insert(*spend);
        }
    }

    Ok(block_spends)
}

/// Reject double-spends of nullifers:
/// - both within this block, and
/// - one in this block, and the other already committed to the finalized state.
///
/// `NullifierT` can be a sprout, sapling, or orchard nullifier.
///
/// Returns the unique set of nullifiers in `block_reveals`.
///
/// "A nullifier MUST NOT repeat either within a transaction,
/// or across transactions in a valid blockchain.
/// Sprout and Sapling and Orchard nullifiers are considered disjoint,
/// even if they have the same bit pattern."
///
/// https:///zips.z.cash/protocol/protocol.pdf#nullifierset
///
/// "A transaction is not valid if it would have added a nullifier
/// to the nullifier set that already exists in the set"
///
/// https://zips.z.cash/protocol/protocol.pdf#commitmentsandnullifiers
pub fn nullifier_double_spends<NullifierT: Eq + PartialEq + Hash + IntoDisk>(
    db: &rocksdb::DB,
    cf: &ColumnFamily,
    block_reveals: Vec<NullifierT>,
) -> Result<HashSet<NullifierT>, BoxError> {
    // check for in-block duplicates
    let unique_spend_count = block_reveals.iter().unique().count();
    if unique_spend_count < block_reveals.len() {
        return Err(format!(
            "duplicate {} spends within block",
            std::any::type_name::<NullifierT>()
        )
        .into());
    }

    // check for block-state duplicates
    for spend in block_reveals.iter() {
        if db.get_cf(cf, spend.as_bytes())?.is_some() {
            return Err(format!(
                "duplicate {} spend already committed to finalized state",
                std::any::type_name::<NullifierT>()
            )
            .into());
        }
    }

    Ok(block_reveals.into_iter().collect())
}
