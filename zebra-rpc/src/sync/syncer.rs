//! The [`TrustedChainSync`] task: maintains a non-finalized state and the chain-tip channels by
//! committing blocks streamed from a co-located trusted Zebra node's indexer gRPC.

use std::{collections::VecDeque, net::SocketAddr, time::Duration};

use tokio::task::JoinHandle;
use tower::BoxError;
use zebra_chain::{
    block::{self, Height},
    diagnostic::task::WaitForPanics,
};
use zebra_state::{
    ChainTipBlock, ChainTipChange, ChainTipSender, CheckpointVerifiedBlock, LatestChainTip,
    NonFinalizedState, SemanticallyVerifiedBlock, ValidateContextError, ZebraDb,
};

use crate::indexer::indexer_client::IndexerClient;

/// HTTP/2 keep-alive ping interval for the indexer gRPC connection, so a half-open connection is
/// detected promptly instead of hanging a stream read indefinitely.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// How long to wait for a keep-alive ping response before treating the connection as dead.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// Syncs the best chain from a trusted Zebra node's indexer gRPC, maintaining a non-finalized state
/// and publishing chain tip changes.
#[derive(Debug)]
pub struct TrustedChainSync {
    /// gRPC client for calling Zebra's indexer methods.
    pub indexer_rpc_client: IndexerClient<tonic::transport::Channel>,
    /// The read state service.
    pub(super) db: ZebraDb,
    /// The non-finalized state - currently only contains the best chain.
    pub(super) non_finalized_state: NonFinalizedState,
    /// The chain tip sender for updating [`LatestChainTip`] and [`ChainTipChange`].
    pub(super) chain_tip_sender: ChainTipSender,
    /// The non-finalized state sender, for updating the [`ReadStateService`](zebra_state::ReadStateService) when the non-finalized best chain changes.
    pub(super) non_finalized_state_sender: tokio::sync::watch::Sender<NonFinalizedState>,
    /// Streamed blocks that can't attach yet because our finalized (secondary) state hasn't caught
    /// up to their parent. Held in ascending height order and drained once a catch-up exposes the
    /// lowest one's parent; from there the rest chain off the non-finalized state.
    pub(super) pending_blocks: VecDeque<SemanticallyVerifiedBlock>,
    /// The hash of the block that most recently failed to commit, to avoid re-logging while it waits.
    pub(super) last_failed_commit_hash: Option<block::Hash>,
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

        let mut syncer = Self {
            indexer_rpc_client: IndexerClient::new(channel),
            db,
            non_finalized_state,
            chain_tip_sender,
            non_finalized_state_sender,
            pending_blocks: VecDeque::new(),
            last_failed_commit_hash: None,
        };

        let sync_task = tokio::spawn(async move {
            syncer.sync().await;
        });

        Ok((latest_chain_tip, chain_tip_change, sync_task))
    }

    /// Syncs the best chain from the node's indexer gRPC `ChainStateChange` stream, which interleaves
    /// new non-finalized blocks with finalized-tip-change signals.
    ///
    /// A single task owns the whole sync, so it is the only caller of `try_catch_up_with_primary` on
    /// the secondary db: the finalized view can't advance between a check and a commit.
    #[tracing::instrument(skip_all)]
    async fn sync(&mut self) {
        let mut stream = None;

        // Publish the finalized tip once up front; from here it advances via the stream's signals.
        let _ = self.db.spawn_try_catch_up_with_primary().await;
        self.publish_finalized_tip().await;

        loop {
            match self.next_update(&mut stream).await {
                super::stream::ChainStateUpdate::Block(block) => self.commit_or_hold(block).await,
                super::stream::ChainStateUpdate::FinalizedTip => {
                    let _ = self.db.spawn_try_catch_up_with_primary().await;
                    self.publish_finalized_tip().await;
                    self.flush_pending().await;
                }
            }
        }
    }

    /// Queues a streamed block (skipping one we already have), then drains what it can.
    async fn commit_or_hold(&mut self, block: SemanticallyVerifiedBlock) {
        // Skip a block we already have, whether committed (in the non-finalized state) or still
        // held (in the pending queue). The pending check matters when the stream is torn down
        // mid-hold and resubscribes with empty resume-tips: the server re-streams every still-held
        // block, and without this they'd accumulate as duplicate forks and could stall the drain.
        if self.non_finalized_state.any_chain_contains(&block.hash)
            || self
                .pending_blocks
                .iter()
                .any(|pending| pending.hash == block.hash)
        {
            tracing::debug!(hash = ?block.hash, "block already known, skipping");
            return;
        }

        self.pending_blocks.push_back(block);
        self.flush_pending().await;
    }

    /// Catches up to the primary, then commits as many queued blocks as can attach, in height order.
    ///
    /// Stops at the first block whose parent isn't finalized yet, keeping it for the next
    /// finalized-tip signal instead of tearing the subscription down. A block the secondary has
    /// since finalized is dropped rather than committed (which would fail as a duplicate-effects
    /// error) — reliable here because this task is the sole catch-up caller, so the finalized view
    /// is stable across the drain.
    async fn flush_pending(&mut self) {
        if self.pending_blocks.is_empty() {
            return;
        }

        let _ = self.db.spawn_try_catch_up_with_primary().await;

        while let Some(block) = self.pending_blocks.front().cloned() {
            if Some(block.height) <= self.db.finalized_tip_height() {
                self.pending_blocks.pop_front();
                continue;
            }

            let hash = block.hash;
            match self.commit(block) {
                Ok(()) => {
                    self.pending_blocks.pop_front();
                    self.last_failed_commit_hash = None;
                }
                Err(error) => {
                    // Log only on transitions so a block waiting for catch-up doesn't saturate logs.
                    if self.last_failed_commit_hash != Some(hash) {
                        tracing::warn!(?error, ?hash, "block can't attach yet, will retry");
                        self.last_failed_commit_hash = Some(hash);
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

    /// Publishes the secondary's finalized tip, unless we already track a non-finalized chain.
    ///
    /// Once we do, the published tip is the (higher) non-finalized tip and `set_finalized_tip` is a
    /// no-op, so this skips the db read and never drags the reported tip backwards.
    async fn publish_finalized_tip(&mut self) {
        if self.non_finalized_state.best_chain().is_some() {
            return;
        }

        if let Some(tip_block) = finalized_chain_tip_block(&self.db).await {
            self.chain_tip_sender.set_finalized_tip(tip_block);
        }
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
