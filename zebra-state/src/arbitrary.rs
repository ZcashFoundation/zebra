//! Randomised data generation for state data.

use std::sync::Arc;

use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    block::{self, Block},
    transaction::Transaction,
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    request::ContextuallyVerifiedBlock, service::chain_tip::ChainTipBlock, CheckpointVerifiedBlock,
    SemanticallyVerifiedBlock,
};

/// Mocks computation done during semantic validation
pub trait Prepare {
    /// Runs block semantic validation computation, and returns the result.
    /// Test-only method.
    fn prepare(self) -> SemanticallyVerifiedBlock;
}

impl Prepare for Arc<Block> {
    fn prepare(self) -> SemanticallyVerifiedBlock {
        let block = self;
        let hash = block.hash();
        let height = block.coinbase_height().unwrap();
        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs =
            transparent::new_ordered_outputs_with_height(&block, height, &transaction_hashes);

        SemanticallyVerifiedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        }
    }
}

impl<T> From<T> for ChainTipBlock
where
    T: Prepare,
{
    fn from(block: T) -> Self {
        block.prepare().into()
    }
}

impl SemanticallyVerifiedBlock {
    /// Returns a [`ContextuallyVerifiedBlock`] created from this block,
    /// with fake zero-valued spent UTXOs.
    ///
    /// Only for use in tests.
    #[cfg(test)]
    pub fn test_with_zero_spent_utxos(&self) -> ContextuallyVerifiedBlock {
        ContextuallyVerifiedBlock::test_with_zero_spent_utxos(self)
    }

    /// Returns a [`ContextuallyVerifiedBlock`] created from this block,
    /// using a fake chain value pool change.
    ///
    /// Only for use in tests.
    #[cfg(test)]
    pub fn test_with_chain_pool_change(
        &self,
        fake_chain_value_pool_change: ValueBalance<NegativeAllowed>,
    ) -> ContextuallyVerifiedBlock {
        ContextuallyVerifiedBlock::test_with_chain_pool_change(self, fake_chain_value_pool_change)
    }

    /// Returns a [`ContextuallyVerifiedBlock`] created from this block,
    /// with no chain value pool change.
    ///
    /// Only for use in tests.
    #[cfg(test)]
    pub fn test_with_zero_chain_pool_change(&self) -> ContextuallyVerifiedBlock {
        ContextuallyVerifiedBlock::test_with_zero_chain_pool_change(self)
    }
}

impl ContextuallyVerifiedBlock {
    /// Create a block that's ready for non-finalized `Chain` contextual
    /// validation, using a [`SemanticallyVerifiedBlock`] and fake zero-valued spent UTXOs.
    ///
    /// Only for use in tests.
    pub fn test_with_zero_spent_utxos(block: impl Into<SemanticallyVerifiedBlock>) -> Self {
        let block = block.into();

        let zero_output = transparent::Output {
            value: Amount::zero(),
            lock_script: transparent::Script::new(&[]),
        };

        let zero_utxo = transparent::OrderedUtxo::new(zero_output, block::Height(1), 1);

        let zero_spent_utxos = block
            .block
            .transactions
            .iter()
            .map(AsRef::as_ref)
            .flat_map(Transaction::inputs)
            .flat_map(transparent::Input::outpoint)
            .map(|outpoint| (outpoint, zero_utxo.clone()))
            .collect();

        ContextuallyVerifiedBlock::with_block_and_spent_utxos(block, zero_spent_utxos)
            .expect("all UTXOs are provided with zero values")
    }

    /// Create a [`ContextuallyVerifiedBlock`] from a [`Block`] or [`SemanticallyVerifiedBlock`],
    /// using a fake chain value pool change.
    ///
    /// Only for use in tests.
    pub fn test_with_chain_pool_change(
        block: impl Into<SemanticallyVerifiedBlock>,
        fake_chain_value_pool_change: ValueBalance<NegativeAllowed>,
    ) -> Self {
        let SemanticallyVerifiedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        } = block.into();

        Self {
            block,
            hash,
            height,
            new_outputs: new_outputs.clone(),
            // Just re-use the outputs we created in this block, even though that's incorrect.
            //
            // TODO: fix the tests, and stop adding unrelated inputs and outputs.
            spent_outputs: new_outputs,
            transaction_hashes,
            chain_value_pool_change: fake_chain_value_pool_change,
        }
    }

    /// Create a [`ContextuallyVerifiedBlock`] from a [`Block`] or [`SemanticallyVerifiedBlock`],
    /// with no chain value pool change.
    ///
    /// Only for use in tests.
    pub fn test_with_zero_chain_pool_change(block: impl Into<SemanticallyVerifiedBlock>) -> Self {
        Self::test_with_chain_pool_change(block, ValueBalance::zero())
    }
}

impl CheckpointVerifiedBlock {
    /// Create a block that's ready to be committed to the finalized state,
    /// using a precalculated [`block::Hash`] and [`block::Height`].
    ///
    /// This is a test-only method, prefer [`CheckpointVerifiedBlock::with_hash`].
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn with_hash_and_height(
        block: Arc<Block>,
        hash: block::Hash,
        height: block::Height,
    ) -> Self {
        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs =
            transparent::new_ordered_outputs_with_height(&block, height, &transaction_hashes);

        Self(SemanticallyVerifiedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        })
    }
}
