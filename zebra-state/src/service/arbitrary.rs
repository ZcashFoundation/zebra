use std::sync::Arc;

use proptest::{
    num::usize::BinarySearch,
    prelude::*,
    strategy::{NewTree, ValueTree},
    test_runner::TestRunner,
};

use zebra_chain::{
    block::{self, Block, Height},
    fmt::SummaryDebug,
    parameters::NetworkUpgrade,
    LedgerState,
};

use crate::{arbitrary::Prepare, constants};

use super::*;

/// The chain length for state proptests.
///
/// Most generated chains will contain transparent spends at or before this height.
///
/// This height was chosen as a tradeoff between chains with no transparent spends,
/// and chains which spend outputs created by previous spends.
///
/// See [`block::arbitrary::PREVOUTS_CHAIN_HEIGHT`] for details.
pub const MAX_PARTIAL_CHAIN_BLOCKS: usize =
    constants::MIN_TRANSPARENT_COINBASE_MATURITY as usize + block::arbitrary::PREVOUTS_CHAIN_HEIGHT;

#[derive(Debug)]
pub struct PreparedChainTree {
    chain: Arc<SummaryDebug<Vec<PreparedBlock>>>,
    count: BinarySearch,
    network: Network,
}

impl ValueTree for PreparedChainTree {
    type Value = (
        Arc<SummaryDebug<Vec<PreparedBlock>>>,
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
    chain: std::sync::Mutex<Option<(Network, Arc<SummaryDebug<Vec<PreparedBlock>>>)>>,
    // the height from which to start the chain. If None, starts at the genesis block
    start_height: Option<Height>,
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
        let height = (std::cmp::max(main_height, test_height) + 1).expect("must be valid");

        PreparedChain {
            start_height: Some(height),
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
            let ledger_strategy = match self.start_height {
                Some(start_height) => {
                    LedgerState::height_strategy(start_height, NetworkUpgrade::Nu5, None, false)
                }
                None => LedgerState::genesis_strategy(NetworkUpgrade::Nu5, None, false),
            };

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
            *chain = Some((network, Arc::new(SummaryDebug(blocks))));
        }

        let chain = chain.clone().expect("should be generated");
        // `count` must be 1 less since the first block is used to build the
        // history tree.
        let count = (1..chain.1.len() - 1).new_tree(runner)?;
        Ok(PreparedChainTree {
            chain: chain.1,
            count,
            network: chain.0,
        })
    }
}
