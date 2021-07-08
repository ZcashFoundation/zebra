//! Unspent transparent output data structures and functions.

use std::collections::HashMap;

use crate::{
    block::{self, Block},
    transaction, transparent,
};

/// An unspent `transparent::Output`, with accompanying metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Utxo {
    /// The output itself.
    pub output: transparent::Output,
    /// The height at which the output was created.
    pub height: block::Height,
    /// Whether the output originated in a coinbase transaction.
    pub from_coinbase: bool,
}

/// A [`Utxo`], and the index of its transaction within its block.
///
/// This extra index is used to check that spends come after outputs,
/// when a new output and its spend are both in the same block.
///
/// The extra index is only used during block verification,
/// so it does not need to be sent to the state.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct OrderedUtxo {
    /// An unspent transaction output.
    pub utxo: Utxo,
    /// The index of the transaction that created the output, in the block at `height`.
    ///
    /// Used to make sure that transaction can only spend outputs
    /// that were created earlier in the chain.
    ///
    /// Note: this is different from `OutPoint.index`,
    /// which is the index of the output in its transaction.
    pub tx_index_in_block: usize,
}

impl OrderedUtxo {
    /// Create a new ordered UTXO from its fields.
    pub fn new(
        output: transparent::Output,
        height: block::Height,
        from_coinbase: bool,
        tx_index_in_block: usize,
    ) -> OrderedUtxo {
        let utxo = Utxo {
            output,
            height,
            from_coinbase,
        };

        OrderedUtxo {
            utxo,
            tx_index_in_block,
        }
    }
}

/// Compute an index of [`Utxo`]s, given an index of [`OrderedUtxo`]s.
pub fn utxos_from_ordered_utxos(
    ordered_utxos: &HashMap<transparent::OutPoint, OrderedUtxo>,
) -> HashMap<transparent::OutPoint, Utxo> {
    ordered_utxos
        .iter()
        .map(|(out_point, ordered_utxo)| (*out_point, ordered_utxo.utxo.clone()))
        .collect()
}

/// Compute an index of newly created [`Utxo`]s, given a block and a
/// list of precomputed transaction hashes.
pub fn new_outputs(
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, Utxo> {
    utxos_from_ordered_utxos(&new_ordered_outputs(block, transaction_hashes))
}

/// Compute an index of newly created [`OrderedUtxo`]s, given a block and a
/// list of precomputed transaction hashes.
pub fn new_ordered_outputs(
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, OrderedUtxo> {
    let mut new_ordered_outputs = HashMap::default();
    let height = block.coinbase_height().expect("block has coinbase height");
    for (tx_index_in_block, (transaction, hash)) in block
        .transactions
        .iter()
        .zip(transaction_hashes.iter().cloned())
        .enumerate()
    {
        let from_coinbase = transaction.is_coinbase();
        for (index, output) in transaction.outputs().iter().cloned().enumerate() {
            let index = index as u32;
            new_ordered_outputs.insert(
                transparent::OutPoint { hash, index },
                OrderedUtxo::new(output, height, from_coinbase, tx_index_in_block),
            );
        }
    }

    new_ordered_outputs
}
