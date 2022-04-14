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
    request::ContextuallyValidBlock, service::chain_tip::ChainTipBlock, FinalizedBlock,
    PreparedBlock,
};

/// Mocks computation done during semantic validation
pub trait Prepare {
    fn prepare(self) -> PreparedBlock;
}

impl Prepare for Arc<Block> {
    fn prepare(self) -> PreparedBlock {
        let block = self;
        let hash = block.hash();
        let height = block.coinbase_height().unwrap();
        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs =
            transparent::new_ordered_outputs_with_height(&block, height, &transaction_hashes);

        PreparedBlock {
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

impl From<PreparedBlock> for ChainTipBlock {
    fn from(prepared: PreparedBlock) -> Self {
        let PreparedBlock {
            block,
            hash,
            height,
            new_outputs: _,
            transaction_hashes,
        } = prepared;
        Self {
            hash,
            height,
            time: block.header.time,
            transaction_hashes,
            previous_block_hash: block.header.previous_block_hash,
        }
    }
}

impl PreparedBlock {
    /// Returns a [`ContextuallyValidBlock`] created from this block,
    /// with fake zero-valued spent UTXOs.
    ///
    /// Only for use in tests.
    pub fn test_with_zero_spent_utxos(&self) -> ContextuallyValidBlock {
        ContextuallyValidBlock::test_with_zero_spent_utxos(self)
    }

    /// Returns a [`ContextuallyValidBlock`] created from this block,
    /// using a fake chain value pool change.
    ///
    /// Only for use in tests.
    pub fn test_with_chain_pool_change(
        &self,
        fake_chain_value_pool_change: ValueBalance<NegativeAllowed>,
    ) -> ContextuallyValidBlock {
        ContextuallyValidBlock::test_with_chain_pool_change(self, fake_chain_value_pool_change)
    }

    /// Returns a [`ContextuallyValidBlock`] created from this block,
    /// with no chain value pool change.
    ///
    /// Only for use in tests.
    pub fn test_with_zero_chain_pool_change(&self) -> ContextuallyValidBlock {
        ContextuallyValidBlock::test_with_zero_chain_pool_change(self)
    }
}

impl ContextuallyValidBlock {
    /// Create a block that's ready for non-finalized [`Chain`] contextual validation,
    /// using a [`PreparedBlock`] and fake zero-valued spent UTXOs.
    ///
    /// Only for use in tests.
    pub fn test_with_zero_spent_utxos(block: impl Into<PreparedBlock>) -> Self {
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

        ContextuallyValidBlock::with_block_and_spent_utxos(block, zero_spent_utxos)
            .expect("all UTXOs are provided with zero values")
    }

    /// Create a [`ContextuallyValidBlock`] from a [`Block`] or [`PreparedBlock`],
    /// using a fake chain value pool change.
    ///
    /// Only for use in tests.
    pub fn test_with_chain_pool_change(
        block: impl Into<PreparedBlock>,
        fake_chain_value_pool_change: ValueBalance<NegativeAllowed>,
    ) -> Self {
        let PreparedBlock {
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

    /// Create a [`ContextuallyValidBlock`] from a [`Block`] or [`PreparedBlock`],
    /// with no chain value pool change.
    ///
    /// Only for use in tests.
    pub fn test_with_zero_chain_pool_change(block: impl Into<PreparedBlock>) -> Self {
        Self::test_with_chain_pool_change(block, ValueBalance::zero())
    }
}

impl FinalizedBlock {
    /// Create a block that's ready to be committed to the finalized state,
    /// using a precalculated [`block::Hash`] and [`block::Height`].
    ///
    /// This is a test-only method, prefer [`FinalizedBlock::with_hash`].
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn with_hash_and_height(
        block: Arc<Block>,
        hash: block::Hash,
        height: block::Height,
    ) -> Self {
        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs = transparent::new_outputs_with_height(&block, height, &transaction_hashes);

        Self {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        }
    }
}
