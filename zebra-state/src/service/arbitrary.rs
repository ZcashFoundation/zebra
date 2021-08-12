use std::sync::Arc;

use proptest::{
    num::usize::BinarySearch,
    prelude::*,
    strategy::{NewTree, ValueTree},
    test_runner::TestRunner,
};

use zebra_chain::{
    block::Block, fmt::SummaryDebug, history_tree::HistoryTree, parameters::NetworkUpgrade,
    LedgerState,
};

use crate::arbitrary::Prepare;

use super::*;

pub use zebra_chain::block::arbitrary::MAX_PARTIAL_CHAIN_BLOCKS;

#[derive(Debug)]
pub struct PreparedChainTree {
    chain: Arc<SummaryDebug<Vec<PreparedBlock>>>,
    count: BinarySearch,
    network: Network,
    history_tree: HistoryTree,
}

impl ValueTree for PreparedChainTree {
    type Value = (
        Arc<SummaryDebug<Vec<PreparedBlock>>>,
        <BinarySearch as ValueTree>::Value,
        Network,
        HistoryTree,
    );

    fn current(&self) -> Self::Value {
        (
            self.chain.clone(),
            self.count.current(),
            self.network,
            self.history_tree.clone(),
        )
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
    chain: std::sync::Mutex<Option<(Network, Arc<SummaryDebug<Vec<PreparedBlock>>>, HistoryTree)>>,
    // the strategy for generating LedgerStates. If None, it calls [`LedgerState::genesis_strategy`].
    ledger_strategy: Option<BoxedStrategy<LedgerState>>,
}

impl PreparedChain {
    /// Create a PreparedChain strategy with Heartwood-onward blocks.
    #[cfg(test)]
    pub(crate) fn new_heartwood() -> Self {
        // The history tree only works with Heartwood onward.
        // Since the network will be chosen later, we pick the larger
        // between the mainnet and testnet Heartwood activation heights.
        let main_height = NetworkUpgrade::Heartwood
            .activation_height(Network::Mainnet)
            .expect("must have height");
        let test_height = NetworkUpgrade::Heartwood
            .activation_height(Network::Testnet)
            .expect("must have height");
        let height = std::cmp::max(main_height, test_height);

        PreparedChain {
            ledger_strategy: Some(LedgerState::height_strategy(
                height,
                NetworkUpgrade::Nu5,
                None,
                false,
            )),
            ..Default::default()
        }
    }
}

impl Strategy for PreparedChain {
    type Tree = PreparedChainTree;
    type Value = <PreparedChainTree as ValueTree>::Value;

    fn new_tree(&self, runner: &mut TestRunner) -> NewTree<Self> {
        let mut chain = self.chain.lock().unwrap();
        if chain.is_none() {
            // TODO: use the latest network upgrade (#1974)
            let default_ledger_strategy =
                LedgerState::genesis_strategy(NetworkUpgrade::Nu5, None, false);
            let ledger_strategy = self
                .ledger_strategy
                .as_ref()
                .unwrap_or(&default_ledger_strategy);

            let (network, blocks) = ledger_strategy
                .prop_flat_map(|ledger| {
                    (
                        Just(ledger.network),
                        Block::partial_chain_strategy(
                            ledger,
                            MAX_PARTIAL_CHAIN_BLOCKS,
                            check::utxo::transparent_coinbase_spend,
                        ),
                    )
                })
                .prop_map(|(network, vec)| {
                    (
                        network,
                        vec.iter()
                            .map(|blk| blk.clone().prepare())
                            .collect::<Vec<_>>(),
                    )
                })
                .new_tree(runner)?
                .current();
            // Generate a history tree from the first block
            let history_tree = HistoryTree::from_block(
                network,
                blocks[0].block.clone(),
                // Dummy roots since this is only used for tests
                &Default::default(),
                &Default::default(),
            )
            .expect("history tree should be created");
            *chain = Some((network, Arc::new(SummaryDebug(blocks)), history_tree));
        }

        let chain = chain.clone().expect("should be generated");
        let count = (1..chain.1.len()).new_tree(runner)?;
        Ok(PreparedChainTree {
            chain: chain.1,
            count,
            network: chain.0,
            history_tree: chain.2,
        })
    }
}
