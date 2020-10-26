use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
    time::Instant,
};

use futures::future::FutureExt;
use memory_state::{NonFinalizedState, QueuedBlocks};
use tokio::sync::oneshot;
use tower::{util::BoxService, Service};
use tracing::instrument;
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

use crate::{
    BoxError, CommitBlockError, Config, FinalizedState, Request, Response, ValidateContextError,
};

mod memory_state;
mod utxo;

// todo: put this somewhere
#[derive(Debug)]
pub struct QueuedBlock {
    pub block: Arc<Block>,
    // TODO: add these parameters when we can compute anchors.
    // sprout_anchor: sprout::tree::Root,
    // sapling_anchor: sapling::tree::Root,
    pub rsp_tx: oneshot::Sender<Result<block::Hash, BoxError>>,
}

struct StateService {
    /// Holds data relating to finalized chain state.
    sled: FinalizedState,
    /// Holds data relating to non-finalized chain state.
    mem: NonFinalizedState,
    /// Blocks awaiting their parent blocks for contextual verification.
    queued_blocks: QueuedBlocks,
    /// The set of outpoints with pending requests for their associated transparent::Output
    pending_utxos: utxo::PendingUtxos,
    /// Instant tracking the last time `pending_utxos` was pruned
    last_prune: Instant,
}

impl StateService {
    const PRUNE_INTERVAL: Duration = Duration::from_secs(30);

    pub fn new(config: Config, network: Network) -> Self {
        let sled = FinalizedState::new(&config, network);
        let mem = NonFinalizedState::default();
        let queued_blocks = QueuedBlocks::default();
        let pending_utxos = utxo::PendingUtxos::default();

        Self {
            sled,
            mem,
            queued_blocks,
            pending_utxos,
            last_prune: Instant::now(),
        }
    }

    /// Queue a non finalized block for verification and check if any queued
    /// blocks are ready to be verified and committed to the state.
    ///
    /// This function encodes the logic for [committing non-finalized blocks][1]
    /// in RFC0005.
    ///
    /// [1]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html#committing-non-finalized-blocks
    #[instrument(skip(self, block))]
    fn queue_and_commit_non_finalized_blocks(
        &mut self,
        block: Arc<Block>,
    ) -> oneshot::Receiver<Result<block::Hash, BoxError>> {
        let hash = block.hash();
        let parent_hash = block.header.previous_block_hash;

        if self.contains_committed_block(&block) {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Err("duplicate block".into()));
            return rsp_rx;
        }

        // The queue of blocks maintained by this service acts as a pipeline for
        // blocks waiting for contextual verification. We lazily flush the
        // pipeline here by handling duplicate requests to verify an existing
        // queued block. We handle those duplicate requests by replacing the old
        // channel with the new one and sending an error over the old channel.
        let rsp_rx = if let Some(queued_block) = self.queued_blocks.get_mut(&hash) {
            let (mut rsp_tx, rsp_rx) = oneshot::channel();
            std::mem::swap(&mut queued_block.rsp_tx, &mut rsp_tx);
            let _ = rsp_tx.send(Err("duplicate block".into()));
            rsp_rx
        } else {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            self.queued_blocks.queue(QueuedBlock { block, rsp_tx });
            rsp_rx
        };

        if !self.can_fork_chain_at(&parent_hash) {
            return rsp_rx;
        }

        self.process_queued(parent_hash);

        while self.mem.best_chain_len() > crate::constants::MAX_BLOCK_REORG_HEIGHT {
            let finalized = self.mem.finalize();
            self.sled
                .commit_finalized_direct(finalized)
                .expect("expected that sled errors would not occur");
        }

        self.queued_blocks
            .prune_by_height(self.sled.finalized_tip_height().expect(
            "Finalized state must have at least one block before committing non-finalized state",
        ));

        rsp_rx
    }

    /// Run contextual validation on `block` and add it to the non-finalized
    /// state if it is contextually valid.
    fn validate_and_commit(&mut self, block: Arc<Block>) -> Result<(), CommitBlockError> {
        self.check_contextual_validity(&block)?;
        let parent_hash = block.header.previous_block_hash;

        if self.sled.finalized_tip_hash() == parent_hash {
            self.mem.commit_new_chain(block);
        } else {
            self.mem.commit_block(block);
        }

        Ok(())
    }

    /// Returns `true` if `hash` is a valid previous block hash for new non-finalized blocks.
    fn can_fork_chain_at(&self, hash: &block::Hash) -> bool {
        self.mem.any_chain_contains(hash) || &self.sled.finalized_tip_hash() == hash
    }

    /// Returns true if the given hash has been committed to either the finalized
    /// or non-finalized state.
    fn contains_committed_block(&self, block: &Block) -> bool {
        let hash = block.hash();
        let height = block
            .coinbase_height()
            .expect("coinbase heights should be valid");

        self.mem.any_chain_contains(&hash) || self.sled.get_hash(height) == Some(hash)
    }

    /// Attempt to validate and commit all queued blocks whose parents have
    /// recently arrived starting from `new_parent`, in breadth-first ordering.
    #[instrument(skip(self))]
    fn process_queued(&mut self, new_parent: block::Hash) {
        let mut new_parents = vec![new_parent];

        while let Some(parent) = new_parents.pop() {
            let queued_children = self.queued_blocks.dequeue_children(parent);

            for QueuedBlock { block, rsp_tx } in queued_children {
                let hash = block.hash();
                let result = self
                    .validate_and_commit(block)
                    .map(|()| hash)
                    .map_err(BoxError::from);
                let _ = rsp_tx.send(result);
                new_parents.push(hash);
            }
        }
    }

    /// Check that `block` is contextually valid based on the committed finalized
    /// and non-finalized state.
    fn check_contextual_validity(&mut self, block: &Block) -> Result<(), ValidateContextError> {
        use ValidateContextError::*;

        if block
            .coinbase_height()
            .expect("valid blocks have a coinbase height")
            <= self.sled.finalized_tip_height().expect(
                "finalized state must contain at least one block to use the non-finalized state",
            )
        {
            Err(OrphanedBlock)?;
        }

        // TODO: contextual validation design and implementation
        Ok(())
    }
}

impl Service<Request> for StateService {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let now = Instant::now();

        if self.last_prune + Self::PRUNE_INTERVAL < now {
            self.pending_utxos.prune();
            self.last_prune = now;
        }

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::CommitBlock { block } => {
                self.pending_utxos.check_block(&block);
                let rsp_rx = self.queue_and_commit_non_finalized_blocks(block);

                async move {
                    rsp_rx
                        .await
                        .expect("sender is not dropped")
                        .map(Response::Committed)
                        .map_err(Into::into)
                }
                .boxed()
            }
            Request::CommitFinalizedBlock { block } => {
                let (rsp_tx, rsp_rx) = oneshot::channel();

                self.pending_utxos.check_block(&block);
                self.sled
                    .queue_and_commit_finalized_blocks(QueuedBlock { block, rsp_tx });

                async move {
                    rsp_rx
                        .await
                        .expect("sender is not dropped")
                        .map(Response::Committed)
                        .map_err(Into::into)
                }
                .boxed()
            }
            Request::Depth(hash) => {
                // todo: handle in memory and sled
                let rsp = self.sled.depth(hash).map(Response::Depth);
                async move { rsp }.boxed()
            }
            Request::Tip => {
                // todo: handle in memory and sled
                let rsp = self.sled.tip().map(Response::Tip);
                async move { rsp }.boxed()
            }
            Request::BlockLocator => {
                // todo: handle in memory and sled
                let rsp = self.sled.block_locator().map(Response::BlockLocator);
                async move { rsp }.boxed()
            }
            Request::Transaction(_) => unimplemented!(),
            Request::Block(hash_or_height) => {
                //todo: handle in memory and sled
                let rsp = self.sled.block(hash_or_height).map(Response::Block);
                async move { rsp }.boxed()
            }
            Request::AwaitUtxo(outpoint) => {
                let fut = self.pending_utxos.queue(outpoint);

                if let Some(finalized_utxo) = self.sled.utxo(&outpoint).unwrap() {
                    self.pending_utxos.respond(outpoint, finalized_utxo);
                } else if let Some(non_finalized_utxo) = self.mem.utxo(&outpoint) {
                    self.pending_utxos.respond(outpoint, non_finalized_utxo);
                }

                fut.boxed()
            }
        }
    }
}

/// Initialize a state service from the provided [`Config`].
///
/// Each `network` has its own separate sled database.
///
/// To share access to the state, wrap the returned service in a `Buffer`. It's
/// possible to construct multiple state services in the same application (as
/// long as they, e.g., use different storage locations), but doing so is
/// probably not what you want.
pub fn init(config: Config, network: Network) -> BoxService<Request, Response, BoxError> {
    BoxService::new(StateService::new(config, network))
}
