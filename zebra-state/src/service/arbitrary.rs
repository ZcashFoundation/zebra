//! Arbitrary data generation and test setup for Zebra's state.

use std::{sync::Arc, time::Duration};

use futures::{stream::FuturesUnordered, StreamExt};
use proptest::{
    num::usize::BinarySearch,
    prelude::*,
    strategy::{NewTree, ValueTree},
    test_runner::TestRunner,
};
use tokio::time::timeout;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};

use zebra_chain::{
    block::{Block, Height},
    fmt::{humantime_seconds, SummaryDebug},
    history_tree::HistoryTree,
    parameters::{Network, NetworkUpgrade},
    LedgerState,
};

use crate::{
    arbitrary::Prepare,
    service::{check, ReadStateService, StateService},
    BoxError, ChainTipChange, Config, LatestChainTip, Request, Response, SemanticallyVerifiedBlock,
};

pub use zebra_chain::block::arbitrary::MAX_PARTIAL_CHAIN_BLOCKS;

/// How long we wait for chain tip updates before skipping them.
pub const CHAIN_TIP_UPDATE_WAIT_LIMIT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct PreparedChainTree {
    chain: Arc<SummaryDebug<Vec<SemanticallyVerifiedBlock>>>,
    count: BinarySearch,
    network: Network,
    history_tree: Arc<HistoryTree>,
}

impl ValueTree for PreparedChainTree {
    type Value = (
        Arc<SummaryDebug<Vec<SemanticallyVerifiedBlock>>>,
        <BinarySearch as ValueTree>::Value,
        Network,
        Arc<HistoryTree>,
    );

    fn current(&self) -> Self::Value {
        (
            self.chain.clone(),
            self.count.current(),
            self.network.clone(),
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
    chain: std::sync::Mutex<
        Option<(
            Network,
            Arc<SummaryDebug<Vec<SemanticallyVerifiedBlock>>>,
            Arc<HistoryTree>,
        )>,
    >,
    // the strategy for generating LedgerStates. If None, it calls [`LedgerState::genesis_strategy`].
    ledger_strategy: Option<BoxedStrategy<LedgerState>>,
    generate_valid_commitments: bool,
}

impl PreparedChain {
    /// Create a PreparedChain strategy with Heartwood-onward blocks.
    // dead_code is allowed because the function is called only by tests,
    // but the code is also compiled when proptest-impl is activated.
    #[allow(dead_code)]
    pub(crate) fn new_heartwood() -> Self {
        // The history tree only works with Heartwood onward.
        // Since the network will be chosen later, we pick the larger
        // between the mainnet and testnet Heartwood activation heights.
        let height = Network::iter()
            .map(|network| {
                NetworkUpgrade::Heartwood
                    .activation_height(&network)
                    .expect("must have height")
            })
            .max()
            .expect("Network::iter() must return non-empty iterator");

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

    /// Transform the strategy to use valid commitments in the block.
    ///
    /// This is slower so it should be used only when needed.
    #[allow(dead_code)]
    pub(crate) fn with_valid_commitments(mut self) -> Self {
        self.generate_valid_commitments = true;
        self
    }

    /// Set the ledger strategy for the prepared chain.
    #[allow(dead_code)]
    pub(crate) fn with_ledger_strategy(
        mut self,
        ledger_strategy: BoxedStrategy<LedgerState>,
    ) -> Self {
        self.ledger_strategy = Some(ledger_strategy);
        self
    }
}

impl Strategy for PreparedChain {
    type Tree = PreparedChainTree;
    type Value = <PreparedChainTree as ValueTree>::Value;

    #[allow(clippy::unwrap_in_result)]
    fn new_tree(&self, runner: &mut TestRunner) -> NewTree<Self> {
        let mut chain = self.chain.lock().unwrap();
        if chain.is_none() {
            // TODO: use the latest network upgrade (#1974)
            let default_ledger_strategy =
                LedgerState::genesis_strategy(None, NetworkUpgrade::Nu5, None, false);
            let ledger_strategy = self
                .ledger_strategy
                .as_ref()
                .unwrap_or(&default_ledger_strategy);

            let (network, blocks) = ledger_strategy
                .prop_flat_map(|ledger| {
                    (
                        Just(ledger.network.clone()),
                        Block::partial_chain_strategy(
                            ledger,
                            MAX_PARTIAL_CHAIN_BLOCKS,
                            check::utxo::transparent_coinbase_spend,
                            self.generate_valid_commitments,
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
                &network,
                blocks[0].block.clone(),
                // Dummy roots since this is only used for tests
                &Default::default(),
                &Default::default(),
            )
            .expect("history tree should be created");
            *chain = Some((
                network,
                Arc::new(SummaryDebug(blocks)),
                Arc::new(history_tree),
            ));
        }

        let chain = chain.clone().expect("should be generated");
        // The generated chain should contain at least two blocks:
        // 1. the zeroth genesis block, and
        // 2. a first block.
        let count = (2..chain.1.len()).new_tree(runner)?;
        Ok(PreparedChainTree {
            chain: chain.1,
            count,
            network: chain.0,
            history_tree: chain.2,
        })
    }
}

/// Initialize a state service with blocks, and return:
/// - a read-write [`StateService`]
/// - a read-only [`ReadStateService`]
/// - a [`LatestChainTip`]
/// - a [`ChainTipChange`] tracker
pub async fn populated_state(
    blocks: impl IntoIterator<Item = Arc<Block>>,
    network: &Network,
) -> (
    Buffer<BoxService<Request, Response, BoxError>, Request>,
    ReadStateService,
    LatestChainTip,
    ChainTipChange,
) {
    let requests = blocks
        .into_iter()
        .map(|block| Request::CommitCheckpointVerifiedBlock(block.into()));

    // TODO: write a test that checks the finalized to non-finalized transition with UTXOs,
    //       and set max_checkpoint_height and checkpoint_verify_concurrency_limit correctly.
    let (state, read_state, latest_chain_tip, mut chain_tip_change) =
        StateService::new(Config::ephemeral(), network, Height::MAX, 0);
    let mut state = Buffer::new(BoxService::new(state), 1);

    let mut responses = FuturesUnordered::new();

    for request in requests {
        let rsp = state.ready().await.unwrap().call(request);
        responses.push(rsp);
    }

    while let Some(rsp) = responses.next().await {
        // Wait for the block result and the chain tip update,
        // which both happen in a separate thread from this one.
        rsp.expect("unexpected block commit failure");

        // Wait for the chain tip update
        if let Err(timeout_error) = timeout(
            CHAIN_TIP_UPDATE_WAIT_LIMIT,
            chain_tip_change.wait_for_tip_change(),
        )
        .await
        .map(|change_result| change_result.expect("unexpected chain tip update failure"))
        {
            debug!(
                timeout = ?humantime_seconds(CHAIN_TIP_UPDATE_WAIT_LIMIT),
                ?timeout_error,
                "timeout waiting for chain tip change after committing block"
            );
        }
    }

    (state, read_state, latest_chain_tip, chain_tip_change)
}
