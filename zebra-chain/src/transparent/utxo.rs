//! Unspent transparent output data structures and functions.

use std::{collections::HashMap, convert::TryInto};

use crate::{
    block::{self, Block},
    transaction::{self, Transaction},
    transparent,
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

/// A restriction that must be checked before spending a transparent output of a
/// coinbase transaction.
///
/// See [`CoinbaseSpendRestriction::check_spend`] for the consensus rules.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub enum CoinbaseSpendRestriction {
    /// The UTXO is spent in a transaction with one or more transparent outputs
    SomeTransparentOutputs,

    /// The UTXO is spent in a transaction which only has shielded outputs
    OnlyShieldedOutputs {
        /// The height at which the UTXO is spent
        spend_height: block::Height,
    },
}

/// Compute an index of [`Utxo`]s, given an index of [`OrderedUtxo`]s.
pub fn utxos_from_ordered_utxos(
    ordered_utxos: HashMap<transparent::OutPoint, OrderedUtxo>,
) -> HashMap<transparent::OutPoint, Utxo> {
    ordered_utxos
        .into_iter()
        .map(|(outpoint, ordered_utxo)| (outpoint, ordered_utxo.utxo))
        .collect()
}

/// Compute an index of [`Output`]s, given an index of [`Utxo`]s.
pub(crate) fn outputs_from_utxos(
    utxos: HashMap<transparent::OutPoint, Utxo>,
) -> HashMap<transparent::OutPoint, transparent::Output> {
    utxos
        .into_iter()
        .map(|(outpoint, utxo)| (outpoint, utxo.output))
        .collect()
}

/// Compute an index of newly created [`Utxo`]s, given a block and a
/// list of precomputed transaction hashes.
pub fn new_outputs(
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, Utxo> {
    utxos_from_ordered_utxos(new_ordered_outputs(block, transaction_hashes))
}

/// Compute an index of newly created [`OrderedUtxo`]s, given a block and a
/// list of precomputed transaction hashes.
pub fn new_ordered_outputs(
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, OrderedUtxo> {
    let mut new_ordered_outputs = HashMap::new();
    let height = block.coinbase_height().expect("block has coinbase height");

    for (tx_index_in_block, (transaction, hash)) in block
        .transactions
        .iter()
        .zip(transaction_hashes.iter().cloned())
        .enumerate()
    {
        new_ordered_outputs.extend(new_transaction_ordered_outputs(
            transaction,
            hash,
            tx_index_in_block,
            height,
        ));
    }

    new_ordered_outputs
}

/// Compute an index of newly created [`OrderedUtxo`]s, given a transaction,
/// its precomputed transaction hash, the transaction's index in its block,
/// and the block's height.
///
/// This function is only intended for use in tests.
pub(crate) fn new_transaction_ordered_outputs(
    transaction: &Transaction,
    hash: transaction::Hash,
    tx_index_in_block: usize,
    height: block::Height,
) -> HashMap<transparent::OutPoint, OrderedUtxo> {
    let mut new_ordered_outputs = HashMap::new();

    let from_coinbase = transaction.has_valid_coinbase_transaction_inputs();
    for (output_index_in_transaction, output) in transaction.outputs().iter().cloned().enumerate() {
        let output_index_in_transaction = output_index_in_transaction
            .try_into()
            .expect("unexpectedly large number of outputs");
        new_ordered_outputs.insert(
            transparent::OutPoint {
                hash,
                index: output_index_in_transaction,
            },
            OrderedUtxo::new(output, height, from_coinbase, tx_index_in_block),
        );
    }

    new_ordered_outputs
}
