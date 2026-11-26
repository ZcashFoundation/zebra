//! Block commits that reuse the early mining work the state computed for the same block, on a
//! replayed Regtest chain.

use std::{sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Result};
use tower::{buffer::Buffer, util::BoxService, ServiceExt};

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Block, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network},
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;
use zebra_test::{args, prelude::*};

use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs, testdir},
    launch::{wait_for_mempool_activation, ZebradTestDirExt},
    regtest::MiningRpcMethods,
};

/// The number of blocks that `commits_reuse_early_mining_work` mines after the genesis block.
const MINED_BLOCKS: u32 = 6;

/// How long the replay waits for the router to verify and commit a block.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// A buffered state service, as [`zebra_state::init_test_services`] returns it.
type State = Buffer<
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>,
    zebra_state::Request,
>;

/// A buffered block verifier router, as [`zebra_consensus::router::init_test`] returns it.
type Router = Buffer<
    BoxService<zebra_consensus::Request, block::Hash, zebra_consensus::RouterError>,
    zebra_consensus::Request,
>;

/// Returns a Regtest network where NU5 is active from block 1, so every block after the
/// genesis block commits to the chain history and its authorizing data.
fn network() -> Network {
    Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    )
}

/// Mines `count` blocks on `network` with a child `zebrad`, and returns them in height order.
async fn mine_blocks(network: &Network, count: u32) -> Result<Vec<Arc<Block>>> {
    let mut config = os_assigned_rpc_port_config(false, network)?;
    config.mempool.debug_enable_at_height = Some(0);

    let mut block_builder = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    let rpc_address = read_listen_addr_from_logs(&mut block_builder, OPENED_RPC_ENDPOINT_MSG)?;

    let client = RpcRequestClient::new(rpc_address);
    wait_for_mempool_activation(&client).await?;
    let mut blocks = Vec::new();
    for expected_height in 1..=count {
        let (block, height) = client.block_from_template(network).await?;
        assert_eq!(height.0, expected_height);
        client.submit_block(block.clone()).await?;
        blocks.push(Arc::new(block));
    }

    block_builder.kill(false)?;
    let output = block_builder.wait_with_output()?;
    output.assert_failure()?.assert_was_killed()?;

    Ok(blocks)
}

/// An in-process block verifier router and state, like the ones a node's syncer drives. The
/// state computes early mining work for the blocks the router verifies.
struct Replay {
    /// The block verifier router, which commits blocks to `state`.
    router: Router,

    /// The state service.
    state: State,

    /// The state only computes early mining work while something is subscribed to it.
    _mining_candidates: zebra_state::MiningCandidateChanges,
}

impl Replay {
    /// Returns a new router and state for `network`, with no blocks.
    async fn new(network: &Network) -> Result<Self> {
        let (state, read_state, _latest_chain_tip, _chain_tip_change) =
            zebra_state::init_test_services(network).await;
        let (router, _transaction_verifier, _background_tasks, _max_checkpoint_height) =
            zebra_consensus::router::init_test(
                zebra_consensus::Config::default(),
                network,
                state.clone(),
            )
            .await;

        let zebra_state::ReadResponse::MiningCandidateChanges(mining_candidates) = read_state
            .oneshot(zebra_state::ReadRequest::MiningCandidateChanges)
            .await
            .map_err(|error| eyre!(error))?
        else {
            unreachable!("wrong response for ReadRequest::MiningCandidateChanges");
        };

        Ok(Self {
            router,
            state,
            _mining_candidates: mining_candidates,
        })
    }

    /// Verifies `block` and commits it to the state, or returns an error if that takes longer
    /// than [`COMMIT_TIMEOUT`].
    async fn commit(&self, block: Arc<Block>) -> Result<block::Hash> {
        tokio::time::timeout(
            COMMIT_TIMEOUT,
            self.router
                .clone()
                .oneshot(zebra_consensus::Request::Commit(block)),
        )
        .await
        .map_err(|_| eyre!("the router didn't commit the block within {COMMIT_TIMEOUT:?}"))?
        .map_err(|error| eyre!(error))
    }

    /// Returns the state's best tip.
    async fn tip(&self) -> Result<Option<(Height, block::Hash)>> {
        match self
            .state
            .clone()
            .oneshot(zebra_state::Request::Tip)
            .await
            .map_err(|error| eyre!(error))?
        {
            zebra_state::Response::Tip(tip) => Ok(tip),
            _ => unreachable!("wrong response for Request::Tip"),
        }
    }
}

/// The commit of each replayed Regtest block that got early mining work reuses that work.
///
/// The state only reuses a mining candidate's work for the block object it was computed for, and
/// only if the block write task lends it to the block's commit. So this fails if the router, the
/// block verifier or the state's queue replaces a block's `Arc`, or if the block write task stops
/// lending the work.
// The in-process state service commits blocks with `block_in_place`, which requires a
// multi-threaded runtime.
#[tokio::test(flavor = "multi_thread")]
async fn commits_reuse_early_mining_work() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = network();
    let blocks = mine_blocks(&network, MINED_BLOCKS).await?;
    let replay = Replay::new(&network).await?;

    let before = zebra_state::early_mining_work_counts();

    // Block 1 gets no early work: the state only computes it once its block write task takes
    // non-finalized blocks, which starts with block 1's commit.
    for block in std::iter::once(regtest_genesis_block()).chain(blocks.iter().cloned()) {
        let hash = block.hash();
        assert_eq!(replay.commit(block).await?, hash);
    }

    let after = zebra_state::early_mining_work_counts();
    tracing::info!(?before, ?after, "early mining work counts");
    let candidates = after.candidates - before.candidates;
    let reused_trees = after.reused_trees - before.reused_trees;
    let reused_history_trees = after.reused_history_trees - before.reused_history_trees;
    let skipped_commitment_checks =
        after.skipped_commitment_checks - before.skipped_commitment_checks;

    assert_eq!(
        replay.tip().await?,
        blocks
            .last()
            .map(|block| (Height(MINED_BLOCKS), block.hash())),
    );

    // The state turns down a block's request for early work while the previous mining
    // candidate's task is still running, so a block can get none. Comparing totals allows for
    // that.
    assert!(
        candidates >= 1,
        "the blocks after block 1 get early mining work"
    );
    assert_eq!(
        reused_trees, candidates,
        "each candidate's commit reuses its note commitment trees"
    );
    assert_eq!(
        reused_history_trees, candidates,
        "each candidate's commit reuses its history tree"
    );
    assert_eq!(
        skipped_commitment_checks, candidates,
        "each candidate's commit skips the header commitment check the candidate did"
    );

    Ok(())
}
