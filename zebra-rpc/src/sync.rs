//! Syncer task for maintaining a non-finalized state in Zebra's ReadStateService and updating `ChainTipSender` via RPCs

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::task::JoinHandle;
use tonic::{Status, Streaming};
use tower::BoxError;
use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::BytesInDisplayOrder,
};
use zebra_state::{
    spawn_init_read_only, ChainTipBlock, ChainTipChange, ChainTipSender, CheckpointVerifiedBlock,
    HashOrHeight, LatestChainTip, NonFinalizedState, ReadStateService, SemanticallyVerifiedBlock,
    ValidateContextError, ZebraDb,
};

use zebra_chain::diagnostic::task::WaitForPanics;

use crate::indexer::{
    block_request, indexer_client::IndexerClient, BlockAndHash, BlockRequest, Empty,
    NonFinalizedStateChangeRequest,
};

/// How long to wait between calls to `subscribe_to_non_finalized_state_change` when it returns an error.
const POLL_DELAY: Duration = Duration::from_secs(5);

/// How long to wait for a message on a gRPC subscription stream before assuming the stream is dead
/// and re-subscribing.
///
/// Generous, because legitimate gaps between blocks/tip changes can be several minutes; this is a
/// backstop against a wedged connection that the keep-alive ping below doesn't catch. Re-subscribing
/// is cheap and resumes from the syncer's current chain tips, so an occasional false trigger during
/// a quiet period is harmless.
const STREAM_MESSAGE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// HTTP/2 keep-alive ping interval for the indexer gRPC connection, so a half-open connection is
/// detected promptly instead of hanging a stream read indefinitely.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// How long to wait for a keep-alive ping response before treating the connection as dead.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait before re-subscribing after a block fails to commit to the non-finalized state.
///
/// A block can persistently fail to commit (e.g. [`ValidateContextError::NotReadyToBeCommitted`])
/// when the secondary finalized state hasn't yet caught up with the primary. Without this delay,
/// re-subscribing immediately turns that into a full-speed busy loop that saturates the logs.
const COMMIT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// How long to wait for a single `get_block` fetch while bridging the finalized gap before giving
/// up. A single block fetch from a co-located node should return promptly, so this bounds a wedged
/// connection without false-triggering; the bridge retries on the next subscription.
const GET_BLOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait to establish a gRPC subscription stream before assuming the request is wedged
/// and retrying. The subscription handshake should complete promptly.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);

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

async fn update_finalized_chain_tip(
    db: ZebraDb,
    mut indexer_rpc_client: IndexerClient<tonic::transport::Channel>,
    mut finalized_chain_tip_sender: ChainTipSender,
) {
    let mut chain_tip_change_stream = None;

    loop {
        let Some(ref mut chain_tip_change) = chain_tip_change_stream else {
            chain_tip_change_stream = match tokio::time::timeout(
                SUBSCRIBE_TIMEOUT,
                indexer_rpc_client.chain_tip_change(Empty {}),
            )
            .await
            {
                Ok(Ok(response)) => Some(response.into_inner()),
                Ok(Err(err)) => {
                    tracing::warn!(?err, "failed to subscribe to chain tip changes");
                    tokio::time::sleep(POLL_DELAY).await;
                    None
                }
                Err(_) => {
                    tracing::warn!("timed out subscribing to chain tip changes");
                    tokio::time::sleep(POLL_DELAY).await;
                    None
                }
            };

            continue;
        };

        // The message is only a signal that the primary's best chain advanced. We publish our own
        // finalized tip below, not the primary's (non-finalized) best tip, so the hash is unused.
        match tokio::time::timeout(STREAM_MESSAGE_TIMEOUT, chain_tip_change.message()).await {
            Ok(Ok(Some(_block_hash_and_height))) => {}
            Ok(Ok(None)) => {
                tracing::warn!("chain_tip_change stream ended unexpectedly");
                chain_tip_change_stream = None;
                continue;
            }
            Ok(Err(err)) => {
                tracing::warn!(?err, "error receiving chain tip change");
                chain_tip_change_stream = None;
                continue;
            }
            Err(_) => {
                tracing::debug!("chain tip change stream timed out, re-subscribing");
                chain_tip_change_stream = None;
                continue;
            }
        }

        // Catch the secondary's finalized state up to the primary, then publish its finalized tip.
        //
        // This keeps the finalized tip current while the secondary is still catching up. Once
        // `TrustedChainSync::sync()` commits its first non-finalized block, the chain tip sender
        // switches to the non-finalized tip and these finalized updates become no-ops, so this task
        // doesn't need to stop itself once the syncer has taken over.
        if let Err(error) = db.spawn_try_catch_up_with_primary().await {
            tracing::debug!(
                ?error,
                "failed to catch up to the primary database while updating the finalized tip"
            );
            continue;
        }

        if let Some(tip_block) = finalized_chain_tip_block(&db).await {
            finalized_chain_tip_sender.set_finalized_tip(tip_block);
        }
    }
}

/// Reads the finalized tip block from the secondary db instance and converts it to a
/// [`ChainTipBlock`].
async fn finalized_chain_tip_block(db: &ZebraDb) -> Option<ChainTipBlock> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let (height, hash) = db.tip()?;
        db.block(height.into())
            .map(|block| CheckpointVerifiedBlock::with_hash(block, hash))
            .map(ChainTipBlock::from)
    })
    .wait_for_panics()
    .await
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
        let channel =
            tonic::transport::Endpoint::from_shared(format!("http://{indexer_rpc_address}"))?
                .keep_alive_while_idle(true)
                .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
                .keep_alive_timeout(KEEPALIVE_TIMEOUT)
                .connect()
                .await?;
        let indexer_rpc_client = IndexerClient::new(channel);
        let finalized_chain_tip_sender = chain_tip_sender.finalized_sender();

        let mut syncer = Self {
            indexer_rpc_client: indexer_rpc_client.clone(),
            db: db.clone(),
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
        };

        // Spawn a task to send finalized chain tip changes to the chain tip change and latest chain tip channels.
        tokio::spawn(async move {
            update_finalized_chain_tip(db, indexer_rpc_client, finalized_chain_tip_sender).await
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
        // The hash of the block that most recently failed to commit, used to avoid re-logging the
        // same warning at full rate while a block persistently fails to commit.
        let mut last_failed_commit_hash = None;
        self.try_catch_up_with_primary().await;
        if let Some(finalized_tip_block) = finalized_chain_tip_block(&self.db).await {
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

            let message = match tokio::time::timeout(
                STREAM_MESSAGE_TIMEOUT,
                non_finalized_state_change.message(),
            )
            .await
            {
                Ok(Ok(Some(block_and_hash))) => block_and_hash,
                Ok(Ok(None)) => {
                    tracing::warn!("non-finalized state change stream ended unexpectedly");
                    non_finalized_blocks_listener = None;
                    continue;
                }
                Ok(Err(err)) => {
                    tracing::warn!(?err, "error receiving non-finalized state change");
                    non_finalized_blocks_listener = None;
                    continue;
                }
                Err(_) => {
                    tracing::debug!("non-finalized state change stream timed out, re-subscribing");
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
                tracing::warn!(?hash, "non-finalized state already contains block");
                continue;
            }

            let block = SemanticallyVerifiedBlock::with_hash(Arc::new(block), hash);
            match self.try_commit(block.clone()).await {
                Ok(()) => {
                    last_failed_commit_hash = None;
                }
                Err(error) => {
                    // Only log on transitions to avoid saturating the logs when the same block
                    // persistently fails to commit (e.g. when the secondary finalized state hasn't
                    // caught up with the primary yet).
                    if last_failed_commit_hash != Some(hash) {
                        tracing::warn!(
                            ?error,
                            ?hash,
                            "failed to commit block to non-finalized state"
                        );
                        last_failed_commit_hash = Some(hash);
                    }

                    non_finalized_blocks_listener = None;

                    // Back off before re-subscribing so a persistently failing block doesn't turn
                    // re-subscription into a full-speed busy loop.
                    tokio::time::sleep(COMMIT_RETRY_DELAY).await;
                }
            };
        }
    }

    async fn try_commit(
        &mut self,
        block: SemanticallyVerifiedBlock,
    ) -> Result<(), ValidateContextError> {
        self.try_catch_up_with_primary().await;

        // When the non-finalized state is empty and the incoming block doesn't build directly on
        // the secondary's finalized tip, the secondary's finalized state is lagging the primary's.
        // The streamed non-finalized blocks start at the primary's finalized tip, which can be
        // several blocks above ours, so the incoming block has no parent to attach to. Bridge that
        // gap by fetching the missing (already-finalized) blocks from the primary, so the incoming
        // block has a contiguous chain to commit onto.
        if self.non_finalized_state.best_chain().is_none()
            && self.db.finalized_tip_hash() != block.block.header.previous_block_hash
        {
            self.fill_finalized_gap(block.height).await;
        }

        self.commit(block)
    }

    /// Commits `block` to the non-finalized state, starting a new chain if it builds on the
    /// finalized tip or extending an existing chain otherwise, prunes newly-finalized blocks, and
    /// publishes the updated chain tip and non-finalized state.
    ///
    /// Updating the channels here (rather than only after `try_commit` returns) means the bridge
    /// blocks committed by [`Self::fill_finalized_gap`] also advance the published chain tip.
    fn commit(&mut self, block: SemanticallyVerifiedBlock) -> Result<(), ValidateContextError> {
        if self.db.finalized_tip_hash() == block.block.header.previous_block_hash {
            self.prune_finalized();
            self.non_finalized_state.commit_new_chain(block, &self.db)?;
        } else {
            self.non_finalized_state.commit_block(block, &self.db)?;
            self.prune_finalized();
        }

        self.update_channels();

        Ok(())
    }

    /// Fetches the blocks between the secondary's finalized tip and `target_height` (exclusive)
    /// from the primary and commits them to the non-finalized state.
    ///
    /// These blocks are finalized on the primary but not yet on the lagging secondary. Committing
    /// them as non-finalized bridge blocks gives the block at `target_height` a contiguous chain to
    /// attach to; they are dropped again by [`Self::prune_finalized`] as the secondary catches up.
    ///
    /// This is best-effort: if a block can't be fetched or committed, it returns early and lets the
    /// caller's commit fail so the block is retried on the next subscription.
    async fn fill_finalized_gap(&mut self, target_height: Height) {
        loop {
            // Try to advance the secondary's finalized state first: if it catches up to the gap on
            // its own, we can stop fetching blocks. This also drops any bridge blocks the secondary
            // has since finalized, so the next height is recomputed from where we actually are.
            self.try_catch_up_with_primary().await;
            self.prune_finalized();

            // The next height to bridge is just above the highest block we already have: the
            // non-finalized tip if we've committed bridge blocks, otherwise the finalized tip.
            let Some(highest) = self
                .non_finalized_state
                .best_tip()
                .map(|(height, _hash)| height)
                .or_else(|| self.db.finalized_tip_height())
            else {
                return;
            };

            let Ok(next_height) = highest.next() else {
                return;
            };

            // Stop once the chain reaches the streamed block's parent, whether by the finalized
            // state catching up or by the bridge blocks we fetched.
            if next_height >= target_height {
                return;
            }

            let (block, hash) = match self.get_block(next_height.into()).await {
                Ok(block_and_hash) => block_and_hash,
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        ?next_height,
                        "failed to fetch a block while bridging the finalized gap; \
                         will retry on the next subscription"
                    );
                    return;
                }
            };

            let block = SemanticallyVerifiedBlock::with_hash(Arc::new(block), hash);
            if let Err(error) = self.commit(block) {
                tracing::warn!(
                    ?error,
                    ?next_height,
                    "failed to commit a block while bridging the finalized gap; \
                     will retry on the next subscription"
                );
                return;
            }
        }
    }

    /// Fetches a single block from the primary by hash or height via the indexer gRPC.
    async fn get_block(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Result<(Block, block::Hash), Status> {
        let request = match hash_or_height {
            HashOrHeight::Hash(hash) => BlockRequest {
                hash_or_height: Some(block_request::HashOrHeight::Hash(
                    hash.bytes_in_display_order().to_vec(),
                )),
            },
            HashOrHeight::Height(height) => BlockRequest {
                hash_or_height: Some(block_request::HashOrHeight::Height(height.0)),
            },
        };

        let response = tokio::time::timeout(
            GET_BLOCK_TIMEOUT,
            self.indexer_rpc_client.clone().get_block(request),
        )
        .await
        .map_err(|_| Status::deadline_exceeded("get_block request timed out"))??;

        response
            .into_inner()
            .decode()
            .ok_or_else(|| Status::internal("failed to decode block from get_block response"))
    }

    /// Calls the `non_finalized_state_change()` method on the indexer gRPC client to subscribe
    /// to non-finalized state changes, and returns the response stream.
    ///
    /// Passes the tip hashes of every chain currently in this syncer's non-finalized state so the
    /// server only streams blocks after the tips we already have, instead of re-sending the whole
    /// non-finalized state on every (re)subscription. When the non-finalized state is empty, the
    /// request carries no tips and the server streams every non-finalized block.
    async fn subscribe_to_non_finalized_state_change(
        &mut self,
    ) -> Result<Streaming<BlockAndHash>, Status> {
        let request = NonFinalizedStateChangeRequest {
            chain_tip_hashes: self
                .non_finalized_state
                .chain_iter()
                .map(|c| c.non_finalized_tip_hash().bytes_in_display_order().to_vec())
                .collect(),
        };

        tokio::time::timeout(
            SUBSCRIBE_TIMEOUT,
            self.indexer_rpc_client
                .clone()
                .non_finalized_state_change(request),
        )
        .await
        .map_err(|_| {
            Status::deadline_exceeded("non_finalized_state_change subscription timed out")
        })?
        .map(|a| a.into_inner())
    }

    /// Tries to catch up to the primary db instance for an up-to-date view of finalized blocks.
    async fn try_catch_up_with_primary(&self) {
        let _ = self.db.spawn_try_catch_up_with_primary().await;
    }

    /// Finalizes any non-finalized blocks that are at or below the finalized tip height.
    ///
    /// The secondary's finalized state follows the primary, so after catching up it may have
    /// advanced past the root of the non-finalized state. This drops those now-finalized blocks so
    /// the non-finalized state only tracks blocks above the finalized tip. Does nothing when the
    /// non-finalized state is empty.
    fn prune_finalized(&mut self) {
        let finalized_tip_height = self.db.finalized_tip_height().unwrap_or(Height::MIN);
        while self
            .non_finalized_state
            .root_height()
            .is_some_and(|root_height| root_height <= finalized_tip_height)
        {
            tracing::trace!("finalizing block past the reorg limit");
            self.non_finalized_state.finalize();
        }
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

        // The outer `?` propagates a `JoinError` if the blocking task panicked or was
        // cancelled, and the inner `?` propagates a `StateInitError` (e.g. a missing
        // read-only database).
        let (read_state, db, non_finalized_state_sender) =
            spawn_init_read_only(config, &network).await??;
        let (latest_chain_tip, chain_tip_change, sync_task) =
            TrustedChainSync::spawn(indexer_rpc_address, db, non_finalized_state_sender).await?;
        Ok((read_state, latest_chain_tip, chain_tip_change, sync_task))
    })
}
