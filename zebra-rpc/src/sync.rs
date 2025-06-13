//! Syncer task for maintaining a non-finalized state in Zebra's ReadStateService and updating `ChainTipSender` via RPCs

use std::{net::SocketAddr, ops::RangeInclusive, sync::Arc, time::Duration};

use futures::{stream::FuturesOrdered, StreamExt};
use tokio::task::JoinHandle;
use tower::BoxError;
use tracing::info;
use zebra_chain::{
    block::{self, Block, Height},
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    serialization::ZcashDeserializeInto,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_state::{
    spawn_init_read_only, ChainTipBlock, ChainTipChange, ChainTipSender, CheckpointVerifiedBlock,
    LatestChainTip, NonFinalizedState, ReadStateService, SemanticallyVerifiedBlock, ZebraDb,
    MAX_BLOCK_REORG_HEIGHT,
};

use zebra_chain::diagnostic::task::WaitForPanics;

use crate::{
    methods::{hex_data::HexData, GetBlockHeightAndHashResponse},
    server,
};

/// How long to wait between calls to `getbestblockheightandhash` when it:
/// - Returns an error, or
/// - Returns the block hash of a block that the read state already contains,
///   (so that there's nothing for the syncer to do except wait for the next chain tip change).
///
/// See the [`TrustedChainSync::wait_for_chain_tip_change()`] method documentation for more information.
const POLL_DELAY: Duration = Duration::from_millis(200);

/// Syncs non-finalized blocks in the best chain from a trusted Zebra node's RPC methods.
#[derive(Debug)]
struct TrustedChainSync {
    /// RPC client for calling Zebra's RPC methods.
    rpc_client: RpcRequestClient,
    /// The read state service.
    db: ZebraDb,
    /// The non-finalized state - currently only contains the best chain.
    non_finalized_state: NonFinalizedState,
    /// The chain tip sender for updating [`LatestChainTip`] and [`ChainTipChange`].
    chain_tip_sender: ChainTipSender,
    /// The non-finalized state sender, for updating the [`ReadStateService`] when the non-finalized best chain changes.
    non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
}

impl TrustedChainSync {
    /// Creates a new [`TrustedChainSync`] with a [`ChainTipSender`], then spawns a task to sync blocks
    /// from the node's non-finalized best chain.
    ///
    /// Returns the [`LatestChainTip`], [`ChainTipChange`], and a [`JoinHandle`] for the sync task.
    pub async fn spawn(
        rpc_address: SocketAddr,
        db: ZebraDb,
        non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
    ) -> (LatestChainTip, ChainTipChange, JoinHandle<()>) {
        let rpc_client = RpcRequestClient::new(rpc_address);
        let non_finalized_state = NonFinalizedState::new(&db.network());
        let (chain_tip_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, &db.network());

        let mut syncer = Self {
            rpc_client,
            db,
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
        };

        let sync_task = tokio::spawn(async move {
            syncer.sync().await;
        });

        (latest_chain_tip, chain_tip_change, sync_task)
    }

    /// Starts syncing blocks from the node's non-finalized best chain and checking for chain tip changes in the finalized state.
    ///
    /// When the best chain tip in Zebra is not available in the finalized state or the local non-finalized state,
    /// gets any unavailable blocks in Zebra's best chain from the RPC server, adds them to the local non-finalized state, then
    /// sends the updated chain tip block and non-finalized state to the [`ChainTipSender`] and non-finalized state sender.
    async fn sync(&mut self) {
        self.try_catch_up_with_primary().await;
        let mut last_chain_tip_hash =
            if let Some(finalized_tip_block) = self.finalized_chain_tip_block().await {
                let last_chain_tip_hash = finalized_tip_block.hash;
                self.chain_tip_sender.set_finalized_tip(finalized_tip_block);
                last_chain_tip_hash
            } else {
                GENESIS_PREVIOUS_BLOCK_HASH
            };

        loop {
            let (target_tip_height, target_tip_hash) =
                self.wait_for_chain_tip_change(last_chain_tip_hash).await;

            info!(
                ?target_tip_height,
                ?target_tip_hash,
                "got a chain tip change"
            );

            if self.is_finalized_tip_change(target_tip_hash).await {
                let block = self.finalized_chain_tip_block().await.expect(
                    "should have genesis block after successful bestblockheightandhash response",
                );

                last_chain_tip_hash = block.hash;
                self.chain_tip_sender.set_finalized_tip(block);
                continue;
            }

            // If the new best chain tip is unavailable in the finalized state, start syncing non-finalized blocks from
            // the non-finalized best chain tip height or finalized tip height.
            let (next_block_height, mut current_tip_hash) =
                self.next_block_height_and_prev_hash().await;

            last_chain_tip_hash = current_tip_hash;

            let rpc_client = self.rpc_client.clone();
            let mut block_futs =
                rpc_client.block_range_ordered(next_block_height..=target_tip_height);

            let should_reset_non_finalized_state = loop {
                let block = match block_futs.next().await {
                    Some(Ok(Some(block)))
                        if block.header.previous_block_hash == current_tip_hash =>
                    {
                        SemanticallyVerifiedBlock::from(block)
                    }
                    // Clear the non-finalized state and re-fetch every block past the finalized tip if:
                    // - the next block's previous block hash doesn't match the expected hash,
                    // - the next block is missing
                    // - the target tip hash is missing from the blocks in `block_futs`
                    // because there was likely a chain re-org/fork.
                    Some(Ok(_)) | None => break true,
                    // If calling the `getblock` RPC method fails with an unexpected error, wait for the next chain tip change
                    // without resetting the non-finalized state.
                    Some(Err(err)) => {
                        tracing::warn!(
                            ?err,
                            "encountered an unexpected error while calling getblock method"
                        );

                        break false;
                    }
                };

                // # Correctness
                //
                // Ensure that the secondary rocksdb instance has caught up to the primary instance
                // before attempting to commit the new block to the non-finalized state. It is sufficient
                // to call this once here, as a new chain tip block has already been retrieved and so
                // we know that the primary rocksdb instance has already been updated.
                self.try_catch_up_with_primary().await;

                let block_hash = block.hash;
                let commit_result = if self.non_finalized_state.chain_count() == 0 {
                    self.non_finalized_state
                        .commit_new_chain(block.clone(), &self.db)
                } else {
                    self.non_finalized_state
                        .commit_block(block.clone(), &self.db)
                };

                // The previous block hash is checked above, if committing the block fails for some reason, try again.
                if let Err(error) = commit_result {
                    tracing::warn!(
                        ?error,
                        ?block_hash,
                        "failed to commit block to non-finalized state"
                    );

                    break false;
                }

                // TODO: Check the finalized tip height and finalize blocks from the non-finalized state until
                //       all non-finalized state chain root previous block hashes match the finalized tip hash.
                while self
                    .non_finalized_state
                    .best_chain_len()
                    .expect("just successfully inserted a non-finalized block above")
                    > MAX_BLOCK_REORG_HEIGHT
                {
                    tracing::trace!("finalizing block past the reorg limit");
                    self.non_finalized_state.finalize();
                }

                self.update_channels(block);
                current_tip_hash = block_hash;
                last_chain_tip_hash = current_tip_hash;

                // If the block hash matches the output from the `getbestblockhash` RPC method, we can wait until
                // the best block hash changes to get the next block.
                if block_hash == target_tip_hash {
                    break false;
                }
            };

            if should_reset_non_finalized_state {
                self.try_catch_up_with_primary().await;
                let block = self.finalized_chain_tip_block().await.expect(
                    "should have genesis block after successful bestblockheightandhash response",
                );

                last_chain_tip_hash = block.hash;
                self.non_finalized_state =
                    NonFinalizedState::new(&self.non_finalized_state.network);
                self.update_channels(block);
            }
        }
    }

    /// Tries to catch up to the primary db instance for an up-to-date view of finalized blocks.
    async fn try_catch_up_with_primary(&self) {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(catch_up_error) = db.try_catch_up_with_primary() {
                tracing::warn!(?catch_up_error, "failed to catch up to primary");
            }
        })
        .wait_for_panics()
        .await
    }

    /// If the non-finalized state is empty, tries to catch up to the primary db instance for
    /// an up-to-date view of finalized blocks.
    ///
    /// Returns true if the non-finalized state is empty and the target hash is in the finalized state.
    async fn is_finalized_tip_change(&self, target_tip_hash: block::Hash) -> bool {
        self.non_finalized_state.chain_count() == 0 && {
            let db = self.db.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(catch_up_error) = db.try_catch_up_with_primary() {
                    tracing::warn!(?catch_up_error, "failed to catch up to primary");
                }
                db.contains_hash(target_tip_hash)
            })
            .wait_for_panics()
            .await
        }
    }

    /// Returns the current tip hash and the next height immediately after the current tip height.
    async fn next_block_height_and_prev_hash(&self) -> (block::Height, block::Hash) {
        if let Some(tip) = self.non_finalized_state.best_tip() {
            Some(tip)
        } else {
            let db = self.db.clone();
            tokio::task::spawn_blocking(move || db.tip())
                .wait_for_panics()
                .await
        }
        .map(|(current_tip_height, current_tip_hash)| {
            (
                current_tip_height.next().expect("should be valid height"),
                current_tip_hash,
            )
        })
        .unwrap_or((Height::MIN, GENESIS_PREVIOUS_BLOCK_HASH))
    }

    /// Reads the finalized tip block from the secondary db instance and converts it to a [`ChainTipBlock`].
    async fn finalized_chain_tip_block(&self) -> Option<ChainTipBlock> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let (height, hash) = db.tip()?;
            db.block(height.into())
                .map(|block| CheckpointVerifiedBlock::with_hash(block, hash))
                .map(ChainTipBlock::from)
        })
        .wait_for_panics()
        .await
    }

    /// Accepts a block hash.
    ///
    /// Polls `getbestblockheightandhash` RPC method until it successfully returns a block hash that is different from the last chain tip hash.
    ///
    /// Returns the node's best block hash.
    async fn wait_for_chain_tip_change(
        &self,
        last_chain_tip_hash: block::Hash,
    ) -> (block::Height, block::Hash) {
        loop {
            let Some(target_height_and_hash) = self
                .rpc_client
                .get_best_block_height_and_hash()
                .await
                .filter(|&(_height, hash)| hash != last_chain_tip_hash)
            else {
                // If `get_best_block_height_and_hash()` returns an error, or returns
                // the current chain tip hash, wait [`POLL_DELAY`], then try again.
                tokio::time::sleep(POLL_DELAY).await;
                continue;
            };

            break target_height_and_hash;
        }
    }

    /// Sends the new chain tip and non-finalized state to the latest chain channels.
    fn update_channels(&mut self, best_tip: impl Into<ChainTipBlock>) {
        // If the final receiver was just dropped, ignore the error.
        let _ = self
            .non_finalized_state_sender
            .send(self.non_finalized_state.clone());
        self.chain_tip_sender
            .set_best_non_finalized_tip(Some(best_tip.into()));
    }
}

/// Accepts a [zebra-state configuration](zebra_state::Config), a [`Network`], and
/// the [`SocketAddr`] of a Zebra node's RPC server.
///
/// Initializes a [`ReadStateService`] and a [`TrustedChainSync`] to update the
/// non-finalized best chain and the latest chain tip.
///
/// Returns a [`ReadStateService`], [`LatestChainTip`], [`ChainTipChange`], and
/// a [`JoinHandle`] for the sync task.
pub fn init_read_state_with_syncer(
    config: zebra_state::Config,
    network: &Network,
    rpc_address: SocketAddr,
) -> tokio::task::JoinHandle<
    Result<
        (
            ReadStateService,
            LatestChainTip,
            ChainTipChange,
            tokio::task::JoinHandle<()>,
        ),
        BoxError,
    >,
> {
    let network = network.clone();
    tokio::spawn(async move {
        if config.ephemeral {
            return Err("standalone read state service cannot be used with ephemeral state".into());
        }

        let (read_state, db, non_finalized_state_sender) =
            spawn_init_read_only(config, &network).await?;
        let (latest_chain_tip, chain_tip_change, sync_task) =
            TrustedChainSync::spawn(rpc_address, db, non_finalized_state_sender).await;
        Ok((read_state, latest_chain_tip, chain_tip_change, sync_task))
    })
}

trait SyncerRpcMethods {
    async fn get_best_block_height_and_hash(&self) -> Option<(block::Height, block::Hash)>;
    async fn get_block(&self, height: u32) -> Result<Option<Arc<Block>>, BoxError>;
    fn block_range_ordered(
        &self,
        height_range: RangeInclusive<Height>,
    ) -> FuturesOrdered<impl std::future::Future<Output = Result<Option<Arc<Block>>, BoxError>>>
    {
        let &Height(start_height) = height_range.start();
        let &Height(end_height) = height_range.end();
        let mut futs = FuturesOrdered::new();

        for height in start_height..=end_height {
            futs.push_back(self.get_block(height));
        }

        futs
    }
}

impl SyncerRpcMethods for RpcRequestClient {
    async fn get_best_block_height_and_hash(&self) -> Option<(block::Height, block::Hash)> {
        self.json_result_from_call("getbestblockheightandhash", "[]")
            .await
            .map(|r: GetBlockHeightAndHashResponse| (r.height(), r.hash()))
            .ok()
    }

    async fn get_block(&self, height: u32) -> Result<Option<Arc<Block>>, BoxError> {
        match self
            .json_result_from_call("getblock", format!(r#"["{}", 0]"#, height))
            .await
        {
            Ok(HexData(raw_block)) => {
                let block = raw_block.zcash_deserialize_into::<Block>()?;
                Ok(Some(Arc::new(block)))
            }
            Err(err)
                if err
                    .downcast_ref::<jsonrpsee_types::ErrorCode>()
                    .is_some_and(|err| {
                        let code: i32 = server::error::LegacyCode::InvalidParameter.into();
                        err.code() == code
                    }) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }
}
