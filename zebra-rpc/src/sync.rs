//! Syncer task for maintaining a non-finalized state in Zebra's ReadStateService and updating `ChainTipSender` via RPCs

use std::{collections::VecDeque, net::SocketAddr, sync::Arc, time::Duration};

use tokio::task::JoinHandle;
use tonic::{Status, Streaming};
use tower::BoxError;
use zebra_chain::{
    block::{self, Height},
    parameters::Network,
    serialization::BytesInDisplayOrder,
};
use zebra_state::{
    spawn_init_read_only, ChainTipBlock, ChainTipChange, ChainTipSender, CheckpointVerifiedBlock,
    LatestChainTip, NonFinalizedState, ReadStateService, SemanticallyVerifiedBlock,
    ValidateContextError, ZebraDb,
};

use zebra_chain::diagnostic::task::WaitForPanics;

use crate::indexer::{
    chain_state_change_message::Change, indexer_client::IndexerClient, ChainStateChangeMessage,
    ChainStateChangeRequest,
};

/// How long to wait between calls to `subscribe_to_chain_state_change` when it returns an error.
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

/// How long to wait to establish a gRPC subscription stream before assuming the request is wedged
/// and retrying. The subscription handshake should complete promptly.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Syncs the best chain from a trusted Zebra node's indexer gRPC, maintaining a non-finalized state
/// and publishing chain tip changes.
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
    /// Creates a new [`TrustedChainSync`] with a [`ChainTipSender`], then spawns a task to sync the
    /// best chain from the node's indexer gRPC.
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

    /// Syncs the best chain from the node's indexer gRPC `ChainStateChange` stream.
    ///
    /// The stream interleaves two kinds of message:
    ///
    /// - a new non-finalized block, which is committed to the local non-finalized state, and
    /// - a finalized-tip-change signal, on which the syncer catches its own finalized (secondary)
    ///   state up to the primary and, until it has a non-finalized chain of its own, publishes its
    ///   finalized tip.
    ///
    /// A single task owns all of this, so it is the only caller of `try_catch_up_with_primary` on
    /// the secondary db: its finalized view can't advance between a check and a commit.
    #[tracing::instrument(skip_all)]
    async fn sync(&mut self) {
        let mut chain_state_change = None;
        // Non-finalized blocks received from the stream that can't attach yet because the secondary
        // finalized state hasn't caught up to their parent. Held in ascending height order and
        // drained once a catch-up makes the lowest one's parent available; from there the rest chain
        // off the non-finalized state and commit too.
        let mut pending: VecDeque<SemanticallyVerifiedBlock> = VecDeque::new();
        // The hash of the block that most recently failed to commit, used to avoid re-logging the
        // same warning at full rate while a block waits for the secondary to catch up.
        let mut last_failed_commit_hash = None;

        self.try_catch_up_with_primary().await;
        if let Some(finalized_tip_block) = finalized_chain_tip_block(&self.db).await {
            self.chain_tip_sender.set_finalized_tip(finalized_tip_block);
        }

        loop {
            let Some(ref mut stream) = chain_state_change else {
                chain_state_change = match self.subscribe_to_chain_state_change().await {
                    Ok(stream) => Some(stream),
                    Err(err) => {
                        tracing::warn!(?err, "failed to subscribe to chain state changes");
                        tokio::time::sleep(POLL_DELAY).await;
                        None
                    }
                };

                continue;
            };

            let message = match tokio::time::timeout(STREAM_MESSAGE_TIMEOUT, stream.message()).await
            {
                Ok(Ok(Some(message))) => message,
                Ok(Ok(None)) => {
                    tracing::warn!("chain state change stream ended unexpectedly");
                    chain_state_change = None;
                    continue;
                }
                Ok(Err(err)) => {
                    tracing::warn!(?err, "error receiving chain state change");
                    chain_state_change = None;
                    continue;
                }
                Err(_) => {
                    tracing::debug!("chain state change stream timed out, re-subscribing");
                    chain_state_change = None;
                    continue;
                }
            };

            match message.change {
                Some(Change::NonFinalizedBlock(block_and_hash)) => {
                    let Some((block, hash)) = block_and_hash.decode() else {
                        tracing::warn!("received malformed non-finalized block message");
                        chain_state_change = None;
                        continue;
                    };

                    if self.non_finalized_state.any_chain_contains(&hash) {
                        // Expected and harmless: on a resumed or multi-chain stream the server can
                        // re-send a block the syncer already has (e.g. a fork's shared ancestors),
                        // so this is logged at debug rather than warn to avoid noise.
                        tracing::debug!(
                            ?hash,
                            "non-finalized state already contains block, skipping"
                        );
                        continue;
                    }

                    pending.push_back(SemanticallyVerifiedBlock::with_hash(Arc::new(block), hash));
                    self.flush_pending(&mut pending, &mut last_failed_commit_hash)
                        .await;
                }

                Some(Change::FinalizedTipChange(_)) => {
                    // The primary's best chain advanced. Catch our finalized state up to it and,
                    // until we have a non-finalized chain of our own, publish our finalized tip.
                    // (Once we do, `set_finalized_tip` is a no-op, so the guard just avoids the read.)
                    self.try_catch_up_with_primary().await;
                    if self.non_finalized_state.best_chain().is_none() {
                        if let Some(tip_block) = finalized_chain_tip_block(&self.db).await {
                            self.chain_tip_sender.set_finalized_tip(tip_block);
                        }
                    }

                    self.flush_pending(&mut pending, &mut last_failed_commit_hash)
                        .await;
                }

                None => {
                    tracing::warn!("received empty chain state change message");
                }
            }
        }
    }

    /// Catches up to the primary, then commits as many held `pending` blocks as can attach, in
    /// ascending height order.
    ///
    /// Stops at the first block whose parent isn't available yet (the secondary is still catching
    /// up); it stays at the front of the queue and is retried on the next finalized-tip change. A
    /// block that the secondary has since finalized is dropped rather than committed, which avoids a
    /// duplicate-effects validation error.
    async fn flush_pending(
        &mut self,
        pending: &mut VecDeque<SemanticallyVerifiedBlock>,
        last_failed_commit_hash: &mut Option<block::Hash>,
    ) {
        if pending.is_empty() {
            return;
        }

        // Catch up once for the whole drain: this task is the only caller, so the finalized view is
        // stable from here until we return, making the `finalized_tip_height` check below reliable.
        self.try_catch_up_with_primary().await;

        while let Some(block) = pending.front() {
            // The secondary finalized this block (e.g. the primary committed it directly via
            // checkpoint sync), so it's no longer a non-finalized block for us: drop it.
            if Some(block.height) <= self.db.finalized_tip_height() {
                pending.pop_front();
                continue;
            }

            let hash = block.hash;
            match self.commit(block.clone()) {
                Ok(()) => {
                    pending.pop_front();
                    *last_failed_commit_hash = None;
                }
                Err(error) => {
                    // The block's parent isn't in the finalized state yet (the secondary is still
                    // catching up). Keep it and retry on the next finalized-tip change. Log only on
                    // transitions to avoid saturating the logs while the same block waits.
                    if *last_failed_commit_hash != Some(hash) {
                        tracing::warn!(
                            ?error,
                            ?hash,
                            "block can't be committed to the non-finalized state yet, will retry"
                        );
                        *last_failed_commit_hash = Some(hash);
                    }

                    break;
                }
            }
        }
    }

    /// Commits `block` to the non-finalized state, starting a new chain if it builds on the
    /// finalized tip or extending an existing chain otherwise, prunes newly-finalized blocks, and
    /// publishes the updated chain tip and non-finalized state.
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

    /// Calls the `chain_state_change()` method on the indexer gRPC client to subscribe to chain
    /// state changes, and returns the response stream.
    ///
    /// Passes the tip hashes of every chain currently in this syncer's non-finalized state so the
    /// server only streams blocks after the tips we already have, instead of re-sending the whole
    /// non-finalized state on every (re)subscription. When the non-finalized state is empty, the
    /// request carries no tips and the server streams every non-finalized block.
    async fn subscribe_to_chain_state_change(
        &mut self,
    ) -> Result<Streaming<ChainStateChangeMessage>, Status> {
        let request = ChainStateChangeRequest {
            chain_tip_hashes: self
                .non_finalized_state
                .chain_iter()
                .map(|c| c.non_finalized_tip_hash().bytes_in_display_order().to_vec())
                .collect(),
        };

        tokio::time::timeout(
            SUBSCRIBE_TIMEOUT,
            self.indexer_rpc_client.clone().chain_state_change(request),
        )
        .await
        .map_err(|_| Status::deadline_exceeded("chain_state_change subscription timed out"))?
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
