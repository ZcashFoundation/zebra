use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::future::{FutureExt, TryFutureExt};
use memory_state::{NonFinalizedState, QueuedBlocks};
use thiserror::Error;
use tokio::sync::oneshot;
use tower::{buffer::Buffer, util::BoxService, Service};
use tracing::instrument;
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

use crate::{BoxError, Config, FinalizedState, Request, Response};

mod memory_state;

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
}

#[derive(Debug, Error)]
#[error("block is not contextually valid")]
struct CommitError(#[from] ValidateContextError);

#[derive(displaydoc::Display, Debug, Error)]
enum ValidateContextError {
    /// block.height is lower than the current finalized height
    OrphanedBlock,
}

impl StateService {
    pub fn new(config: Config, network: Network) -> Self {
        let sled = FinalizedState::new(&config, network);
        let mem = NonFinalizedState::default();
        let queued_blocks = QueuedBlocks::default();

        Self {
            sled,
            mem,
            queued_blocks,
        }
    }

    /// Queue a non finalized block for verification and check if any queued
    /// blocks are ready to be verified and committed to the state.
    ///
    /// This function encodes the logic for [committing non-finalized blocks][1]
    /// in RFC0005.
    ///
    /// [1]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html#committing-non-finalized-blocks
    #[instrument(skip(self, new))]
    fn queue(&mut self, new: QueuedBlock) {
        let parent_hash = new.block.header.previous_block_hash;

        self.queued_blocks.queue(new);

        if !self.contains(&parent_hash) {
            return;
        }

        self.process_queued(parent_hash);

        while self.mem.best_chain_len() > crate::constants::MAX_BLOCK_REORG_HEIGHT {
            let finalized = self.mem.finalize();
            self.sled
                .commit_finalized_direct(finalized)
                .expect("expected that sled errors would not occur");
        }

        self.queued_blocks
            .prune_by_height(self.sled.finalized_tip_height());
    }

    /// Run contextual validation on `block` and add it to the non-finalized
    /// state if it is contextually valid.
    fn validate_and_commit(&mut self, block: Arc<Block>) -> Result<(), CommitError> {
        self.check_contextual_validity(&block)?;
        let parent_hash = block.header.previous_block_hash;

        if self.sled.finalized_tip_hash() == parent_hash {
            self.mem.commit_new_chain(block);
        } else {
            self.mem.commit_block(block);
        }

        Ok(())
    }

    fn contains(&self, hash: &block::Hash) -> bool {
        self.mem.any_chain_contains(hash) || &self.sled.finalized_tip_hash() == hash
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
                    .map_err(Into::into);
                let _ = rsp_tx.send(result);
                new_parents.push(hash);
            }
        }
    }

    /// Check that `block` is contextually valid based on the committed finalized
    /// and non-finalized state.
    fn check_contextual_validity(&mut self, block: &Block) -> Result<(), ValidateContextError> {
        use ValidateContextError::*;

        if block.coinbase_height().unwrap() <= self.sled.finalized_tip_height() {
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
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::CommitBlock { block } => {
                let (rsp_tx, rsp_rx) = oneshot::channel();

                self.queue(QueuedBlock { block, rsp_tx });

                async move {
                    rsp_rx
                        .await
                        .expect("sender oneshot is not dropped")
                        .map(Response::Committed)
                }
                .boxed()
            }
            Request::CommitFinalizedBlock { block } => {
                let (rsp_tx, rsp_rx) = oneshot::channel();

                self.sled.queue(QueuedBlock { block, rsp_tx });

                async move {
                    rsp_rx
                        .await
                        .expect("sender oneshot is not dropped")
                        .map(Response::Committed)
                }
                .boxed()
            }
            Request::Depth(hash) => {
                // todo: handle in memory and sled
                self.sled.depth(hash).map_ok(Response::Depth).boxed()
            }
            Request::Tip => {
                // todo: handle in memory and sled
                self.sled.tip().map_ok(Response::Tip).boxed()
            }
            Request::BlockLocator => {
                // todo: handle in memory and sled
                self.sled
                    .block_locator()
                    .map_ok(Response::BlockLocator)
                    .boxed()
            }
            Request::Transaction(_) => unimplemented!(),
            Request::Block(hash_or_height) => {
                //todo: handle in memory and sled
                self.sled
                    .block(hash_or_height)
                    .map_ok(Response::Block)
                    .boxed()
            }
        }
    }
}

/// Initialize a state service from the provided [`Config`].
///
/// Each `network` has its own separate sled database.
///
/// The resulting service is clonable, to provide shared access to a common chain
/// state. It's possible to construct multiple state services in the same
/// application (as long as they, e.g., use different storage locations), but
/// doing so is probably not what you want.
pub fn init(
    config: Config,
    network: Network,
) -> Buffer<BoxService<Request, Response, BoxError>, Request> {
    Buffer::new(BoxService::new(StateService::new(config, network)), 3)
}
