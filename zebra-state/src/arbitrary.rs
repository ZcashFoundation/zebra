use std::sync::Arc;

use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    block::{self, Block},
    transaction::Transaction,
    transparent,
    value_balance::ValueBalance,
};

use crate::{request::ContextuallyValidBlock, PreparedBlock};

/// Mocks computation done during semantic validation
pub trait Prepare {
    fn prepare(self) -> PreparedBlock;
}

impl Prepare for Arc<Block> {
    fn prepare(self) -> PreparedBlock {
        let block = self;
        let hash = block.hash();
        let height = block.coinbase_height().unwrap();
        let transaction_hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs = transparent::new_ordered_outputs(&block, transaction_hashes.as_slice());

        PreparedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        }
    }
}

impl PreparedBlock {
    /// Returns a [`ContextuallyValidBlock`] created from this block,
    /// with fake zero-valued spent UTXOs.
    ///
    /// Only for use in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn test_with_zero_spent_utxos(&self) -> ContextuallyValidBlock {
        ContextuallyValidBlock::test_with_zero_spent_utxos(self)
    }

    /// Returns a [`ContextuallyValidBlock`] created from this block,
    /// using a fake chain value pool change.
    ///
    /// Only for use in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
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
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn test_with_zero_chain_pool_change(&self) -> ContextuallyValidBlock {
        ContextuallyValidBlock::test_with_zero_chain_pool_change(self)
    }
}

impl ContextuallyValidBlock {
    /// Create a block that's ready for non-finalized [`Chain`] contextual validation,
    /// using a [`PreparedBlock`] and fake zero-valued spent UTXOs.
    ///
    /// Only for use in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn test_with_zero_spent_utxos(block: impl Into<PreparedBlock>) -> Self {
        let block = block.into();

        let zero_utxo = transparent::Utxo {
            output: transparent::Output {
                value: Amount::zero(),
                lock_script: transparent::Script::new(&[]),
            },
            height: block::Height(1),
            from_coinbase: false,
        };

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
    #[cfg(any(test, feature = "proptest-impl"))]
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
            new_outputs: transparent::utxos_from_ordered_utxos(new_outputs),
            transaction_hashes,
            chain_value_pool_change: fake_chain_value_pool_change,
        }
    }

    /// Create a [`ContextuallyValidBlock`] from a [`Block`] or [`PreparedBlock`],
    /// with no chain value pool change.
    ///
    /// Only for use in tests.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn test_with_zero_chain_pool_change(block: impl Into<PreparedBlock>) -> Self {
        Self::test_with_chain_pool_change(block, ValueBalance::zero())
    }
}
