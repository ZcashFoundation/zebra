use std::sync::Arc;

use zebra_chain::{block::Block, transparent};

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
        let new_unordered_outputs = transparent::new_outputs(&block, transaction_hashes.as_slice());
        let block_value_balance = block
            .chain_value_pool_change(&new_unordered_outputs)
            .unwrap();

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
