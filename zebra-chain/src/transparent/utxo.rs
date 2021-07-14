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

/// Compute an index of newly created transparent outputs, given a block and a
/// list of precomputed transaction hashes.
pub fn new_outputs(
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, Utxo> {
    let mut new_outputs = HashMap::default();
    let height = block.coinbase_height().expect("block has coinbase height");
    for (transaction, hash) in block
        .transactions
        .iter()
        .zip(transaction_hashes.iter().cloned())
    {
        let from_coinbase = transaction.is_coinbase();
        for (index, output) in transaction.outputs().iter().cloned().enumerate() {
            let index = index as u32;
            new_outputs.insert(
                transparent::OutPoint { hash, index },
                Utxo {
                    output,
                    height,
                    from_coinbase,
                },
            );
        }
    }

    new_outputs
}
