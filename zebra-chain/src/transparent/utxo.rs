//! Unspent transparent output data structures and functions.

use std::collections::HashMap;

use crate::{
    block::{self, Block, Height},
    transaction::{self, Transaction},
    transparent,
};

/// An unspent `transparent::Output`, with accompanying metadata.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary, serde::Serialize)
)]
pub struct Utxo {
    /// The output itself.
    pub output: transparent::Output,

    // TODO: replace the height and from_coinbase fields with OutputLocation,
    //       and provide lookup/calculation methods for height and from_coinbase
    //
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
//
// TODO: after modifying UTXO to contain an OutputLocation, replace this type with UTXO
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

impl AsRef<Utxo> for OrderedUtxo {
    fn as_ref(&self) -> &Utxo {
        &self.utxo
    }
}

impl Utxo {
    /// Create a new UTXO from its fields.
    pub fn new(output: transparent::Output, height: block::Height, from_coinbase: bool) -> Utxo {
        Utxo {
            output,
            height,
            from_coinbase,
        }
    }

    /// Create a new UTXO from an output and its transaction location.
    pub fn from_location(
        output: transparent::Output,
        height: block::Height,
        tx_index_in_block: usize,
    ) -> Utxo {
        // Coinbase transactions are always the first transaction in their block,
        // we check the other consensus rules separately.
        let from_coinbase = tx_index_in_block == 0;

        Utxo {
            output,
            height,
            from_coinbase,
        }
    }
}

impl OrderedUtxo {
    /// Create a new ordered UTXO from its fields.
    pub fn new(
        output: transparent::Output,
        height: block::Height,
        tx_index_in_block: usize,
    ) -> OrderedUtxo {
        // Coinbase transactions are always the first transaction in their block,
        // we check the other consensus rules separately.
        let from_coinbase = tx_index_in_block == 0;

        OrderedUtxo {
            utxo: Utxo::new(output, height, from_coinbase),
            tx_index_in_block,
        }
    }

    /// Create a new ordered UTXO from a UTXO and transaction index.
    pub fn from_utxo(utxo: Utxo, tx_index_in_block: usize) -> OrderedUtxo {
        OrderedUtxo {
            utxo,
            tx_index_in_block,
        }
    }
}

/// A restriction that must be checked before spending a transparent output of a
/// coinbase transaction.
///
/// See the function `transparent_coinbase_spend` in `zebra-state` for the
/// consensus rules.
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

/// Compute an index of [`transparent::Output`]s, given an index of [`Utxo`]s.
pub fn outputs_from_utxos(
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

/// Compute an index of newly created [`Utxo`]s, given a block and a
/// list of precomputed transaction hashes.
///
/// This is a test-only function, prefer [`new_outputs`].
#[cfg(any(test, feature = "proptest-impl"))]
pub fn new_outputs_with_height(
    block: &Block,
    height: Height,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, Utxo> {
    utxos_from_ordered_utxos(new_ordered_outputs_with_height(
        block,
        height,
        transaction_hashes,
    ))
}

/// Compute an index of newly created [`OrderedUtxo`]s, given a block and a
/// list of precomputed transaction hashes.
pub fn new_ordered_outputs(
    block: &Block,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, OrderedUtxo> {
    let height = block.coinbase_height().expect("block has coinbase height");

    new_ordered_outputs_with_height(block, height, transaction_hashes)
}

/// Compute an index of newly created [`OrderedUtxo`]s, given a block and a
/// list of precomputed transaction hashes.
///
/// This function is intended for use in this module, and in tests.
/// Prefer [`new_ordered_outputs`].
pub fn new_ordered_outputs_with_height(
    block: &Block,
    height: Height,
    transaction_hashes: &[transaction::Hash],
) -> HashMap<transparent::OutPoint, OrderedUtxo> {
    let mut new_ordered_outputs = HashMap::new();

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
/// This function is only for use in this module, and in tests.
pub fn new_transaction_ordered_outputs(
    transaction: &Transaction,
    hash: transaction::Hash,
    tx_index_in_block: usize,
    height: block::Height,
) -> HashMap<transparent::OutPoint, OrderedUtxo> {
    let mut new_ordered_outputs = HashMap::new();

    for (output_index_in_transaction, output) in transaction.outputs().iter().cloned().enumerate() {
        let output_index_in_transaction = output_index_in_transaction
            .try_into()
            .expect("unexpectedly large number of outputs");

        new_ordered_outputs.insert(
            transparent::OutPoint {
                hash,
                index: output_index_in_transaction,
            },
            OrderedUtxo::new(output, height, tx_index_in_block),
        );
    }

    new_ordered_outputs
}
