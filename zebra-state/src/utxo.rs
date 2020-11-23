// needed to make clippy happy with derive(Arbitrary)
#![allow(clippy::unit_arg)]

use zebra_chain::{block, transparent};

/// An unspent `transparent::Output`, with accompanying metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Utxo {
    /// The output itself.
    pub output: transparent::Output,
    /// The height at which the output was created.
    pub height: block::Height,
    /// Whether the output originated in a coinbase transaction.
    pub from_coinbase: bool,
}

#[cfg(test)]
pub fn new_outputs(block: &block::Block) -> std::collections::HashMap<transparent::OutPoint, Utxo> {
    use std::collections::HashMap;

    let height = block.coinbase_height().expect("block has coinbase height");

    let mut new_outputs = HashMap::default();
    for transaction in &block.transactions {
        let hash = transaction.hash();
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
