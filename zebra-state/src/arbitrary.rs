use std::sync::Arc;

use zebra_chain::{block::Block, transparent, value_balance::ValueBalance};

use crate::PreparedBlock;

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
        // TODO: Call `block.chain_value_pool_change()` with all the needed `Utxo`s.
        // `Utxo`s in `new_outputs` are currently not enough.
        let block_value_balance = ValueBalance::zero();

        PreparedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
            block_value_balance,
        }
    }
}
