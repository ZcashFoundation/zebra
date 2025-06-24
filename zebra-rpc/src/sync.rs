//! Syncer task for maintaining a non-finalized state in Zebra's ReadStateService and updating `ChainTipSender` via RPCs

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::task::JoinHandle;
use tonic::{Status, Streaming};
use tower::BoxError;
use zebra_chain::{block::Height, parameters::Network};
use zebra_state::{
    spawn_init_read_only, ChainTipBlock, ChainTipChange, ChainTipSender, CheckpointVerifiedBlock,
    LatestChainTip, NonFinalizedState, ReadStateService, SemanticallyVerifiedBlock,
    ValidateContextError, ZebraDb,
};

use zebra_chain::diagnostic::task::WaitForPanics;

use crate::indexer::{indexer_client::IndexerClient, BlockAndHash, Empty};

/// How long to wait between calls to `subscribe_to_non_finalized_state_change` when it returns an error.
const POLL_DELAY: Duration = Duration::from_secs(5);

/// Syncs non-finalized blocks in the best chain from a trusted Zebra node's RPC methods.
#[derive(Debug)]
pub struct TrustedChainSync {
    /// gRPC client for calling Zebra's indexer methods.
    pub indexer_rpc_client: IndexerClient<tonic::transport::Channel>,
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
        indexer_rpc_address: SocketAddr,
        db: ZebraDb,
        non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
    ) -> Result<(LatestChainTip, ChainTipChange, JoinHandle<()>), BoxError> {
        let indexer_rpc_client =
            IndexerClient::connect(format!("http://{indexer_rpc_address}")).await?;

        let non_finalized_state = NonFinalizedState::new(&db.network());
        let (chain_tip_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, &db.network());

        let mut syncer = Self {
            indexer_rpc_client,
            db,
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
        };

        let sync_task = tokio::spawn(async move {
            syncer.sync().await;
        });

        Ok((latest_chain_tip, chain_tip_change, sync_task))
    }

    /// Starts syncing blocks from the node's non-finalized best chain and checking for chain tip changes in the finalized state.
    ///
    /// When the best chain tip in Zebra is not available in the finalized state or the local non-finalized state,
    /// gets any unavailable blocks in Zebra's best chain from the RPC server, adds them to the local non-finalized state, then
    /// sends the updated chain tip block and non-finalized state to the [`ChainTipSender`] and non-finalized state sender.
    async fn sync(&mut self) {
        let mut non_finalized_blocks_listener = None;
        self.try_catch_up_with_primary().await;
        if let Some(finalized_tip_block) = self.finalized_chain_tip_block().await {
            self.chain_tip_sender.set_finalized_tip(finalized_tip_block);
        }

        loop {
            let Some(ref mut non_finalized_state_change) = non_finalized_blocks_listener else {
                non_finalized_blocks_listener = match self
                    .subscribe_to_non_finalized_state_change()
                    .await
                {
                    Ok(listener) => Some(listener),
                    Err(err) => {
                        tracing::warn!(?err, "failed to subscribe to non-finalized state changes");
                        tokio::time::sleep(POLL_DELAY).await;
                        None
                    }
                };

                continue;
            };

            let message = match non_finalized_state_change.message().await {
                Ok(Some(block_and_hash)) => block_and_hash,
                Ok(None) => {
                    tracing::warn!("non-finalized state change stream ended unexpectedly");
                    non_finalized_blocks_listener = None;
                    continue;
                }
                Err(err) => {
                    tracing::warn!(?err, "error receiving non-finalized state change");
                    non_finalized_blocks_listener = None;
                    continue;
                }
            };

            let Some((block, hash)) = message.decode() else {
                tracing::warn!("received malformed non-finalized state change message");
                non_finalized_blocks_listener = None;
                continue;
            };

            if self.non_finalized_state.any_chain_contains(&hash) {
                tracing::info!(?hash, "non-finalized state already contains block");
                non_finalized_blocks_listener = None;
                continue;
            }

            let block = SemanticallyVerifiedBlock::with_hash(Arc::new(block), hash);
            match self.try_commit(block.clone()).await {
                Ok(()) => {
                    while self
                        .non_finalized_state
                        .root_height()
                        .expect("just successfully inserted a non-finalized block above")
                        <= self.db.finalized_tip_height().unwrap_or(Height::MIN)
                    {
                        tracing::trace!("finalizing block past the reorg limit");
                        self.non_finalized_state.finalize();
                    }

                    self.update_channels(block)
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        ?hash,
                        "failed to commit block to non-finalized state"
                    );

                    // TODO: Investigate whether it would be correct to ignore some errors here instead of
                    //       trying every block in the non-finalized state again.
                    non_finalized_blocks_listener = None;
                }
            };
        }
    }

    async fn try_commit(
        &mut self,
        block: SemanticallyVerifiedBlock,
    ) -> Result<(), ValidateContextError> {
        self.try_catch_up_with_primary().await;

        if self.db.finalized_tip_hash() == block.block.header.previous_block_hash {
            self.non_finalized_state.commit_new_chain(block, &self.db)
        } else {
            self.non_finalized_state.commit_block(block, &self.db)
        }
    }

    /// Calls `non_finalized_state_change()` method on the indexer gRPC client to subscribe
    /// to non-finalized state changes, and returns the response stream.
    async fn subscribe_to_non_finalized_state_change(
        &mut self,
    ) -> Result<Streaming<BlockAndHash>, Status> {
        self.indexer_rpc_client
            .clone()
            .non_finalized_state_change(Empty {})
            .await
            .map(|a| a.into_inner())
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
    indexer_rpc_address: SocketAddr,
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
            TrustedChainSync::spawn(indexer_rpc_address, db, non_finalized_state_sender).await?;
        Ok((read_state, latest_chain_tip, chain_tip_change, sync_task))
    })
}
