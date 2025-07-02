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
        let non_finalized_state = NonFinalizedState::new(&db.network());
        let (chain_tip_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(None, &db.network());
        let mut indexer_rpc_client =
            IndexerClient::connect(format!("http://{indexer_rpc_address}")).await?;
        let mut finalized_chain_tip_sender = chain_tip_sender.finalized_sender();

        let mut syncer = Self {
            indexer_rpc_client: indexer_rpc_client.clone(),
            db: db.clone(),
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
        };

        // Spawn a task to send finalized chain tip changes to the chain tip change and latest chain tip channels.
        tokio::spawn(async move {
            let mut chain_tip_change_stream = None;

            loop {
                let Some(ref mut chain_tip_change) = chain_tip_change_stream else {
                    chain_tip_change_stream = match indexer_rpc_client
                        .chain_tip_change(Empty {})
                        .await
                        .map(|a| a.into_inner())
                    {
                        Ok(listener) => Some(listener),
                        Err(err) => {
                            tracing::warn!(
                                ?err,
                                "failed to subscribe to non-finalized state changes"
                            );
                            tokio::time::sleep(POLL_DELAY).await;
                            None
                        }
                    };

                    continue;
                };

                let message = match chain_tip_change.message().await {
                    Ok(Some(block_hash_and_height)) => block_hash_and_height,
                    Ok(None) => {
                        tracing::warn!("chain_tip_change stream ended unexpectedly");
                        chain_tip_change_stream = None;
                        continue;
                    }
                    Err(err) => {
                        tracing::warn!(?err, "error receiving chain tip change");
                        chain_tip_change_stream = None;
                        continue;
                    }
                };

                let Some((hash, _height)) = message.try_into_hash_and_height() else {
                    tracing::warn!("failed to convert message into a block hash and height");
                    continue;
                };

                // Skip the chain tip change if catching up to the primary db instance fails.
                if db.spawn_try_catch_up_with_primary().await.is_err() {
                    continue;
                }

                // End the task and let the `TrustedChainSync::sync()` method send non-finalized chain tip updates if
                // the latest chain tip hash is not present in the db.
                let Some(tip_block) = db.block(hash.into()) else {
                    return;
                };

                finalized_chain_tip_sender.set_finalized_tip(Some(
                    SemanticallyVerifiedBlock::with_hash(tip_block, hash).into(),
                ));
            }
        });

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
    #[tracing::instrument(skip_all)]
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

                    self.update_channels();
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
        let _ = self.db.spawn_try_catch_up_with_primary().await;
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
    // TODO: Replace this with the `update_latest_chain_channels()` fn in `write.rs`.
    fn update_channels(&mut self) {
        // If the final receiver was just dropped, ignore the error.
        let _ = self
            .non_finalized_state_sender
            .send(self.non_finalized_state.clone());

        let best_chain = self.non_finalized_state.best_chain().expect("unexpected empty non-finalized state: must commit at least one block before updating channels");

        let tip_block = best_chain
            .tip_block()
            .expect(
                "unexpected empty chain: must commit at least one block before updating channels",
            )
            .clone();

        self.chain_tip_sender
            .set_best_non_finalized_tip(Some(tip_block.into()));
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