use proptest::{
    num::usize::BinarySearch,
    strategy::{NewTree, ValueTree},
    test_runner::TestRunner,
};
use std::sync::Arc;

use zebra_chain::{block::Block, LedgerState};
use zebra_test::prelude::*;

use crate::tests::Prepare;

use super::*;

const MAX_PARTIAL_CHAIN_BLOCKS: usize = 100;

#[derive(Debug)]
pub struct PreparedChainTree {
    chain: Arc<Vec<PreparedBlock>>,
    count: BinarySearch,
}

impl ValueTree for PreparedChainTree {
    type Value = (Arc<Vec<PreparedBlock>>, <BinarySearch as ValueTree>::Value);

    fn current(&self) -> Self::Value {
        (self.chain.clone(), self.count.current())
    }

    fn simplify(&mut self) -> bool {
        self.count.simplify()
    }

    fn complicate(&mut self) -> bool {
        self.count.complicate()
    }
}

#[derive(Debug, Default)]
pub struct PreparedChain {
    // the proptests are threaded (not async), so we want to use a threaded mutex here
    chain: std::sync::Mutex<Option<Arc<Vec<PreparedBlock>>>>,
}

impl Strategy for PreparedChain {
    type Tree = PreparedChainTree;
    type Value = <PreparedChainTree as ValueTree>::Value;

    fn new_tree(&self, runner: &mut TestRunner) -> NewTree<Self> {
        let mut chain = self.chain.lock().unwrap();
        if chain.is_none() {
            // Disable NU5 for now
            // `genesis_strategy(None)` re-enables the default Nu5 override
            let ledger_strategy = LedgerState::genesis_strategy(Canopy);

            let blocks = ledger_strategy
                .prop_flat_map(|ledger| {
                    Block::partial_chain_strategy(ledger, MAX_PARTIAL_CHAIN_BLOCKS)
                })
                .prop_map(|vec| vec.into_iter().map(|blk| blk.prepare()).collect::<Vec<_>>())
                .new_tree(runner)?
                .current();
            *chain = Some(Arc::new(blocks));
        }

        let chain = chain.clone().expect("should be generated");
        let count = (1..chain.len()).new_tree(runner)?;
        Ok(PreparedChainTree { chain, count })
    }
}
