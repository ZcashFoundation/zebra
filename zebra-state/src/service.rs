use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
    time::Instant,
};

use futures::future::FutureExt;
use non_finalized_state::{NonFinalizedState, QueuedBlocks};
use tokio::sync::oneshot;
use tower::{util::BoxService, Service};
use tracing::instrument;
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    transaction,
    transaction::Transaction,
    transparent,
};

use crate::{
    request::HashOrHeight, BoxError, CommitBlockError, Config, Request, Response,
    ValidateContextError,
};

use self::finalized_state::FinalizedState;

mod check;
mod finalized_state;
mod non_finalized_state;
#[cfg(test)]
mod tests;
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
    disk: FinalizedState,
    /// Holds data relating to non-finalized chain state.
    mem: NonFinalizedState,
    /// Blocks awaiting their parent blocks for contextual verification.
    queued_blocks: QueuedBlocks,
    /// The set of outpoints with pending requests for their associated transparent::Output
    pending_utxos: utxo::PendingUtxos,
    /// The configured Zcash network
    network: Network,
    /// Instant tracking the last time `pending_utxos` was pruned
    last_prune: Instant,
}

impl StateService {
    const PRUNE_INTERVAL: Duration = Duration::from_secs(30);

    pub fn new(config: Config, network: Network) -> Self {
        let disk = FinalizedState::new(&config, network);
        let mem = NonFinalizedState::default();
        let queued_blocks = QueuedBlocks::default();
        let pending_utxos = utxo::PendingUtxos::default();

        Self {
            disk,
            mem,
            queued_blocks,
            pending_utxos,
            network,
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
    fn queue_and_commit_non_finalized(
        &mut self,
        block: Arc<Block>,
    ) -> oneshot::Receiver<Result<block::Hash, BoxError>> {
        let hash = block.hash();
        let parent_hash = block.header.previous_block_hash;

        if self.contains_committed_block(&block) {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Err("block is already committed to the state".into()));
            return rsp_rx;
        }

        // Request::CommitBlock contract: a request to commit a block which has
        // been queued but not yet committed to the state fails the older
        // request and replaces it with the newer request.
        let rsp_rx = if let Some(queued_block) = self.queued_blocks.get_mut(&hash) {
            tracing::debug!("replacing older queued request with new request");
            let (mut rsp_tx, rsp_rx) = oneshot::channel();
            std::mem::swap(&mut queued_block.rsp_tx, &mut rsp_tx);
            let _ = rsp_tx.send(Err("replaced by newer request".into()));
            rsp_rx
        } else {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            self.queued_blocks.queue(QueuedBlock { block, rsp_tx });
            rsp_rx
        };

        if !self.can_fork_chain_at(&parent_hash) {
            tracing::trace!("unready to verify, returning early");
            return rsp_rx;
        }

        self.process_queued(parent_hash);

        while self.mem.best_chain_len() > crate::constants::MAX_BLOCK_REORG_HEIGHT {
            tracing::trace!("finalizing block past the reorg limit");
            let finalized = self.mem.finalize();
            self.disk
                .commit_finalized_direct(finalized)
                .expect("expected that disk errors would not occur");
        }

        self.queued_blocks
            .prune_by_height(self.disk.finalized_tip_height().expect(
            "Finalized state must have at least one block before committing non-finalized state",
        ));

        tracing::trace!("finished processing queued block");
        rsp_rx
    }

    /// Run contextual validation on `block` and add it to the non-finalized
    /// state if it is contextually valid.
    fn validate_and_commit(&mut self, block: Arc<Block>) -> Result<(), CommitBlockError> {
        self.check_contextual_validity(&block)?;
        let parent_hash = block.header.previous_block_hash;

        if self.disk.finalized_tip_hash() == parent_hash {
            self.mem.commit_new_chain(block);
        } else {
            self.mem.commit_block(block);
        }

        Ok(())
    }

    /// Returns `true` if `hash` is a valid previous block hash for new non-finalized blocks.
    fn can_fork_chain_at(&self, hash: &block::Hash) -> bool {
        self.mem.any_chain_contains(hash) || &self.disk.finalized_tip_hash() == hash
    }

    /// Returns true if the given hash has been committed to either the finalized
    /// or non-finalized state.
    fn contains_committed_block(&self, block: &Block) -> bool {
        let hash = block.hash();
        let height = block
            .coinbase_height()
            .expect("coinbase heights should be valid");

        self.mem.any_chain_contains(&hash) || self.disk.hash(height) == Some(hash)
    }

    /// Attempt to validate and commit all queued blocks whose parents have
    /// recently arrived starting from `new_parent`, in breadth-first ordering.
    fn process_queued(&mut self, new_parent: block::Hash) {
        let mut new_parents = vec![new_parent];

        while let Some(parent_hash) = new_parents.pop() {
            let queued_children = self.queued_blocks.dequeue_children(parent_hash);

            for QueuedBlock { block, rsp_tx } in queued_children {
                let child_hash = block.hash();
                tracing::trace!(?child_hash, "validating queued child");
                let result = self
                    .validate_and_commit(block)
                    .map(|()| child_hash)
                    .map_err(BoxError::from);
                let _ = rsp_tx.send(result);
                new_parents.push(child_hash);
            }
        }
    }

    /// Check that `block` is contextually valid for the configured network,
    /// based on the committed finalized and non-finalized state.
    fn check_contextual_validity(&mut self, block: &Block) -> Result<(), ValidateContextError> {
        check::block_is_contextually_valid(
            block,
            self.network,
            self.disk.finalized_tip_height(),
            self.chain(block.header.previous_block_hash),
        )?;

        Ok(())
    }

    /// Create a block locator for the current best chain.
    fn block_locator(&self) -> Option<Vec<block::Hash>> {
        let tip_height = self.tip()?.0;

        let heights = crate::util::block_locator_heights(tip_height);
        let mut hashes = Vec::with_capacity(heights.len());

        for height in heights {
            if let Some(hash) = self.hash(height) {
                hashes.push(hash);
            }
        }

        Some(hashes)
    }

    /// Return the tip of the current best chain.
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        self.mem.tip().or_else(|| self.disk.tip())
    }

    /// Return the depth of block `hash` in the current best chain.
    pub fn depth(&self, hash: block::Hash) -> Option<u32> {
        let tip = self.tip()?.0;
        let height = self.mem.height(hash).or_else(|| self.disk.height(hash))?;

        Some(tip.0 - height.0)
    }

    /// Return the block identified by either its `height` or `hash` if it exists
    /// in the current best chain.
    pub fn block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        self.mem
            .block(hash_or_height)
            .or_else(|| self.disk.block(hash_or_height))
    }

    /// Return the transaction identified by `hash` if it exists in the current
    /// best chain.
    pub fn transaction(&self, hash: transaction::Hash) -> Option<Arc<Transaction>> {
        self.mem
            .transaction(hash)
            .or_else(|| self.disk.transaction(hash))
    }

    /// Return the hash for the block at `height` in the current best chain.
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        self.mem.hash(height).or_else(|| self.disk.hash(height))
    }

    /// Return the height for the block at `hash` in any chain.
    pub fn height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.mem
            .height_by_hash(hash)
            .or_else(|| self.disk.height(hash))
    }

    /// Return the utxo pointed to by `outpoint` if it exists in any chain.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Output> {
        self.mem.utxo(outpoint).or_else(|| self.disk.utxo(outpoint))
    }

    /// Return an iterator over the relevant chain of the block identified by
    /// `hash`.
    ///
    /// The block identified by `hash` is included in the chain of blocks yielded
    /// by the iterator.
    pub fn chain(&self, hash: block::Hash) -> Iter<'_> {
        Iter {
            service: self,
            state: IterState::NonFinalized(hash),
        }
    }
}

struct Iter<'a> {
    service: &'a StateService,
    state: IterState,
}

enum IterState {
    NonFinalized(block::Hash),
    Finalized(block::Height),
    Finished,
}

impl Iter<'_> {
    fn next_non_finalized_block(&mut self) -> Option<Arc<Block>> {
        let Iter { service, state } = self;

        let hash = match state {
            IterState::NonFinalized(hash) => *hash,
            IterState::Finalized(_) | IterState::Finished => unreachable!(),
        };

        if let Some(block) = service.mem.block_by_hash(hash) {
            let hash = block.header.previous_block_hash;
            self.state = IterState::NonFinalized(hash);
            Some(block)
        } else {
            None
        }
    }

    fn next_finalized_block(&mut self) -> Option<Arc<Block>> {
        let Iter { service, state } = self;

        let hash_or_height: HashOrHeight = match *state {
            IterState::Finalized(height) => height.into(),
            IterState::NonFinalized(hash) => hash.into(),
            IterState::Finished => unreachable!(),
        };

        if let Some(block) = service.disk.block(hash_or_height) {
            let height = block
                .coinbase_height()
                .expect("valid blocks have a coinbase height");

            if let Some(next_height) = height - 1 {
                self.state = IterState::Finalized(next_height);
            } else {
                self.state = IterState::Finished;
            }

            Some(block)
        } else {
            self.state = IterState::Finished;
            None
        }
    }
}

impl Iterator for Iter<'_> {
    type Item = Arc<Block>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.state {
            IterState::NonFinalized(_) => self
                .next_non_finalized_block()
                .or_else(|| self.next_finalized_block()),
            IterState::Finalized(_) => self.next_finalized_block(),
            IterState::Finished => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl std::iter::FusedIterator for Iter<'_> {}

impl ExactSizeIterator for Iter<'_> {
    fn len(&self) -> usize {
        match self.state {
            IterState::NonFinalized(hash) => self
                .service
                .height_by_hash(hash)
                .map(|height| (height.0 + 1) as _)
                .unwrap_or(0),
            IterState::Finalized(height) => (height.0 + 1) as _,
            IterState::Finished => 0,
        }
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

    #[instrument(name = "state", skip(self, req))]
    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::CommitBlock { block } => {
                metrics::counter!("state.requests", 1, "type" => "commit_block");

                self.pending_utxos.check_block(&block);
                let rsp_rx = self.queue_and_commit_non_finalized(block);

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
                metrics::counter!("state.requests", 1, "type" => "commit_finalized_block");

                let (rsp_tx, rsp_rx) = oneshot::channel();

                self.pending_utxos.check_block(&block);
                self.disk
                    .queue_and_commit_finalized(QueuedBlock { block, rsp_tx });

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
                metrics::counter!("state.requests", 1, "type" => "depth");
                let rsp = Ok(self.depth(hash)).map(Response::Depth);
                async move { rsp }.boxed()
            }
            Request::Tip => {
                metrics::counter!("state.requests", 1, "type" => "tip");
                let rsp = Ok(self.tip()).map(Response::Tip);
                async move { rsp }.boxed()
            }
            Request::BlockLocator => {
                metrics::counter!("state.requests", 1, "type" => "block_locator");
                let rsp = Ok(self.block_locator().unwrap_or_default()).map(Response::BlockLocator);
                async move { rsp }.boxed()
            }
            Request::Transaction(hash) => {
                metrics::counter!("state.requests", 1, "type" => "transaction");
                let rsp = Ok(self.transaction(hash)).map(Response::Transaction);
                async move { rsp }.boxed()
            }
            Request::Block(hash_or_height) => {
                metrics::counter!("state.requests", 1, "type" => "block");
                let rsp = Ok(self.block(hash_or_height)).map(Response::Block);
                async move { rsp }.boxed()
            }
            Request::AwaitUtxo(outpoint) => {
                metrics::counter!("state.requests", 1, "type" => "await_utxo");

                let fut = self.pending_utxos.queue(outpoint);

                if let Some(utxo) = self.utxo(&outpoint) {
                    self.pending_utxos.respond(outpoint, utxo);
                }

                fut.boxed()
            }
        }
    }
}

/// Initialize a state service from the provided [`Config`].
///
/// Each `network` has its own separate on-disk database.
///
/// To share access to the state, wrap the returned service in a `Buffer`. It's
/// possible to construct multiple state services in the same application (as
/// long as they, e.g., use different storage locations), but doing so is
/// probably not what you want.
pub fn init(config: Config, network: Network) -> BoxService<Request, Response, BoxError> {
    BoxService::new(StateService::new(config, network))
}
