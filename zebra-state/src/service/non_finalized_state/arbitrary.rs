use proptest::{
    num::usize::BinarySearch,
    strategy::{NewTree, ValueTree},
    test_runner::TestRunner,
};
use std::sync::Arc;

use zebra_chain::{block::Block, parameters::NetworkUpgrade, LedgerState};
use zebra_test::prelude::*;

use crate::tests::Prepare;

use super::*;

const MAX_PARTIAL_CHAIN_BLOCKS: usize = 102;

#[derive(Debug)]
pub struct PreparedChainTree {
    chain: Arc<Vec<PreparedBlock>>,
    count: BinarySearch,
    network: Network,
}

impl ValueTree for PreparedChainTree {
    type Value = (
        Arc<Vec<PreparedBlock>>,
        <BinarySearch as ValueTree>::Value,
        Network,
    );

    fn current(&self) -> Self::Value {
        (self.chain.clone(), self.count.current(), self.network)
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
    chain: std::sync::Mutex<Option<(Network, Arc<Vec<PreparedBlock>>)>>,
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

            let (network, blocks) = ledger_strategy
                .prop_flat_map(|ledger| {
                    (
                        Just(ledger.network),
                        Block::partial_chain_strategy(ledger, MAX_PARTIAL_CHAIN_BLOCKS),
                    )
                })
                .prop_map(|(network, vec)| {
                    (
                        network,
                        vec.into_iter().map(|blk| blk.prepare()).collect::<Vec<_>>(),
                    )
                })
                .new_tree(runner)?
                .current();
            *chain = Some((network, Arc::new(blocks)));
        }

        let chain = chain.clone().expect("should be generated");
        let count = (1..chain.1.len()).new_tree(runner)?;
        Ok(PreparedChainTree {
            chain: chain.1,
            count,
            network: chain.0,
        })
    }
}
