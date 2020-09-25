use std::{
    collections::BTreeMap,
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::future::{FutureExt, TryFutureExt};
use tokio::sync::oneshot;
use tower::{buffer::Buffer, util::BoxService, Service};
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

use crate::{BoxError, Config, FinalizedState, NonFinalizedState, Request, Response};

// todo: put this somewhere
#[derive(Debug)]
pub struct QueuedBlock {
    pub block: Arc<Block>,
    // TODO: add these parameters when we can compute anchors.
    // sprout_anchor: sprout::tree::Root,
    // sapling_anchor: sapling::tree::Root,
    pub rsp_tx: oneshot::Sender<Result<block::Hash, BoxError>>,
}

/// A queue of blocks, awaiting the arrival of parent blocks.
#[derive(Default)]
struct QueuedBlocks {
    /// Blocks awaiting their parent blocks for contextual verification.
    blocks: HashMap<block::Hash, QueuedBlock>,
    /// Hashes from `queued_blocks`, indexed by parent hash.
    by_parent: HashMap<block::Hash, Vec<block::Hash>>,
    /// Hashes from `queued_blocks`, indexed by block height.
    by_height: BTreeMap<block::Height, Vec<block::Hash>>,
}

impl QueuedBlocks {
    fn queue(&mut self, new: QueuedBlock) {
        let new_hash = new.block.hash();
        let new_height = new
            .block
            .coinbase_height()
            .expect("validated non-finalized blocks have a coinbase height");
        let parent_hash = new.block.header.previous_block_hash;

        self.blocks.insert(new_hash, new);
        self.by_height.entry(new_height).or_default().push(new_hash);
        self.by_parent
            .entry(parent_hash)
            .or_default()
            .push(new_hash);
    }

    fn dequeue_children(&mut self, parent: block::Hash) -> Vec<QueuedBlock> {
        let queued_children = self
            .by_parent
            .remove(&parent)
            .unwrap_or_default()
            .into_iter()
            .map(|hash| {
                self.blocks
                    .remove(&hash)
                    .expect("block is present if its hash is in by_parent")
            })
            .collect::<Vec<_>>();

        for queued in &queued_children {
            let height = queued.block.coinbase_height().unwrap();
            self.by_height.remove(&height);
        }

        queued_children
    }
}

struct StateService {
    /// Holds data relating to finalized chain state.
    sled: FinalizedState,
    /// Holds data relating to non-finalized chain state.
    mem: NonFinalizedState,
    /// Blocks awaiting their parent blocks for contextual verification.
    contextual_queue: QueuedBlocks,
}

enum ValidateContextError {}

impl StateService {
    const REORG_LIMIT: usize = 100;

    pub fn new(config: Config, network: Network) -> Self {
        let sled = FinalizedState::new(&config, network);
        let mem = NonFinalizedState::default();
        let contextual_queue = QueuedBlocks::default();

        Self {
            sled,
            mem,
            contextual_queue,
        }
    }

    fn queue(&mut self, new: QueuedBlock) {
        let parent_hash = new.block.header.previous_block_hash;

        self.contextual_queue.queue(new);

        if !self.contains(&parent_hash) {
            return;
        }

        self.process_queued(parent_hash);

        while self.mem.best_chain_len() > Self::REORG_LIMIT {
            let finalized = self.mem.finalize();
            self.sled
                .commit_finalized_direct(finalized)
                .expect("sled would never do us dirty like that");
        }
    }

    fn validate_and_commit(&mut self, ready: QueuedBlock) {
        let validity = self.check_contextual_validity(&ready.block);

        if let Err(_err) = validity {
            todo!("wrap with error and send back on channel")
        }

        let block_hash = ready.block.hash();

        if self.finalized_tip_hash() == &block_hash {
            self.mem.commit_new_chain(ready.block);
            todo!("send success back through channel");
        } else {
            self.mem.commit_block(ready.block);
            todo!("send success back through channel");
        }
    }

    fn check_contextual_validity(&mut self, _block: &Block) -> Result<(), ValidateContextError> {
        // Draw the rest of the owl
        Ok(())
    }

    fn contains(&self, hash: &block::Hash) -> bool {
        self.mem.any_chain_contains(hash) || self.sled.contains(hash)
    }

    fn finalized_tip_hash(&self) -> &block::Hash {
        unimplemented!()
    }

    fn process_queued(&mut self, new_parent: block::Hash) {
        let mut new_parents = vec![new_parent];

        while let Some(parent) = new_parents.pop() {
            let queued_children = self.contextual_queue.dequeue_children(parent);

            for ready in queued_children {
                let hash = ready.block.hash();
                self.validate_and_commit(ready);
                new_parents.push(hash);
            }
        }
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
