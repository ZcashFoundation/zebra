use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use check::difficulty::POW_MEDIAN_BLOCK_SPAN;
use futures::future::FutureExt;
use non_finalized_state::{NonFinalizedState, QueuedBlocks};
use tokio::sync::oneshot;
use tower::{util::BoxService, Service};
use tracing::instrument;
use zebra_chain::{
    block::{self, Block},
    parameters::POW_AVERAGING_WINDOW,
    parameters::{Network, NetworkUpgrade},
    transaction,
    transaction::Transaction,
    transparent,
};

use crate::{
    request::HashOrHeight, BoxError, CommitBlockError, Config, FinalizedBlock, PreparedBlock,
    Request, Response, Utxo, ValidateContextError,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;
mod check;
mod finalized_state;
mod non_finalized_state;
mod pending_utxos;
#[cfg(test)]
mod tests;

use self::{finalized_state::FinalizedState, pending_utxos::PendingUtxos};

pub type QueuedBlock = (
    PreparedBlock,
    oneshot::Sender<Result<block::Hash, BoxError>>,
);
pub type QueuedFinalized = (
    FinalizedBlock,
    oneshot::Sender<Result<block::Hash, BoxError>>,
);

struct StateService {
    /// Holds data relating to finalized chain state.
    disk: FinalizedState,
    /// Holds data relating to non-finalized chain state.
    mem: NonFinalizedState,
    /// Blocks awaiting their parent blocks for contextual verification.
    queued_blocks: QueuedBlocks,
    /// The set of outpoints with pending requests for their associated transparent::Output
    pending_utxos: PendingUtxos,
    /// The configured Zcash network
    network: Network,
    /// Instant tracking the last time `pending_utxos` was pruned
    last_prune: Instant,
}

impl StateService {
    const PRUNE_INTERVAL: Duration = Duration::from_secs(30);

    pub fn new(config: Config, network: Network) -> Self {
        let disk = FinalizedState::new(&config, network);

        let mem = NonFinalizedState {
            network,
            ..Default::default()
        };
        let queued_blocks = QueuedBlocks::default();
        let pending_utxos = PendingUtxos::default();

        let state = Self {
            disk,
            mem,
            queued_blocks,
            pending_utxos,
            network,
            last_prune: Instant::now(),
        };

        tracing::info!("starting legacy chain check");
        if let Some(tip) = state.best_tip() {
            if let Some(nu5_activation_height) = NetworkUpgrade::Nu5.activation_height(network) {
                if legacy_chain_check(
                    nu5_activation_height,
                    state.any_ancestor_blocks(tip.1),
                    state.network,
                )
                .is_err()
                {
                    let legacy_db_path = Some(state.disk.path().to_path_buf());
                    panic!(
                        "Cached state contains a legacy chain. \
                        An outdated Zebra version did not know about a recent network upgrade, \
                        so it followed a legacy chain using outdated rules. \
                        Hint: Delete your database, and restart Zebra to do a full sync. \
                        Database path: {:?}",
                        legacy_db_path,
                    );
                }
            }
        }
        tracing::info!("no legacy chain found");

        state
    }

    /// Queue a non finalized block for verification and check if any queued
    /// blocks are ready to be verified and committed to the state.
    ///
    /// This function encodes the logic for [committing non-finalized blocks][1]
    /// in RFC0005.
    ///
    /// [1]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html#committing-non-finalized-blocks
    #[instrument(level = "debug", skip(self, prepared))]
    fn queue_and_commit_non_finalized(
        &mut self,
        prepared: PreparedBlock,
    ) -> oneshot::Receiver<Result<block::Hash, BoxError>> {
        tracing::debug!(block = %prepared.block, "queueing block for contextual verification");
        let parent_hash = prepared.block.header.previous_block_hash;

        if self.mem.any_chain_contains(&prepared.hash) || self.disk.hash(prepared.height).is_some()
        {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Err("block is already committed to the state".into()));
            return rsp_rx;
        }

        // Request::CommitBlock contract: a request to commit a block which has
        // been queued but not yet committed to the state fails the older
        // request and replaces it with the newer request.
        let rsp_rx = if let Some((_, old_rsp_tx)) = self.queued_blocks.get_mut(&prepared.hash) {
            tracing::debug!("replacing older queued request with new request");
            let (mut rsp_tx, rsp_rx) = oneshot::channel();
            std::mem::swap(old_rsp_tx, &mut rsp_tx);
            let _ = rsp_tx.send(Err("replaced by newer request".into()));
            rsp_rx
        } else {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            self.queued_blocks.queue((prepared, rsp_tx));
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
                .commit_finalized_direct(finalized, "best non-finalized chain root")
                .expect("expected that disk errors would not occur");
        }

        self.queued_blocks
            .prune_by_height(self.disk.finalized_tip_height().expect(
            "Finalized state must have at least one block before committing non-finalized state",
        ));

        tracing::trace!("finished processing queued block");
        rsp_rx
    }

    /// Run contextual validation on the prepared block and add it to the
    /// non-finalized state if it is contextually valid.
    fn validate_and_commit(&mut self, prepared: PreparedBlock) -> Result<(), CommitBlockError> {
        self.check_contextual_validity(&prepared)?;
        let parent_hash = prepared.block.header.previous_block_hash;

        if self.disk.finalized_tip_hash() == parent_hash {
            self.mem.commit_new_chain(prepared)?;
        } else {
            self.mem.commit_block(prepared)?;
        }

        Ok(())
    }

    /// Returns `true` if `hash` is a valid previous block hash for new non-finalized blocks.
    fn can_fork_chain_at(&self, hash: &block::Hash) -> bool {
        self.mem.any_chain_contains(hash) || &self.disk.finalized_tip_hash() == hash
    }

    /// Attempt to validate and commit all queued blocks whose parents have
    /// recently arrived starting from `new_parent`, in breadth-first ordering.
    fn process_queued(&mut self, new_parent: block::Hash) {
        let mut new_parents = vec![new_parent];

        while let Some(parent_hash) = new_parents.pop() {
            let queued_children = self.queued_blocks.dequeue_children(parent_hash);

            for (child, rsp_tx) in queued_children {
                let child_hash = child.hash;
                tracing::trace!(?child_hash, "validating queued child");
                let result = self
                    .validate_and_commit(child)
                    .map(|()| child_hash)
                    .map_err(BoxError::from);
                let _ = rsp_tx.send(result);
                new_parents.push(child_hash);
            }
        }
    }

    /// Check that the prepared block is contextually valid for the configured
    /// network, based on the committed finalized and non-finalized state.
    fn check_contextual_validity(
        &mut self,
        prepared: &PreparedBlock,
    ) -> Result<(), ValidateContextError> {
        let relevant_chain = self.any_ancestor_blocks(prepared.block.header.previous_block_hash);
        assert!(relevant_chain.len() >= POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN,
                "contextual validation requires at least 28 (POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN) blocks");

        check::block_is_contextually_valid(
            prepared,
            self.network,
            self.disk.finalized_tip_height(),
            relevant_chain,
        )?;

        Ok(())
    }

    /// Create a block locator for the current best chain.
    fn block_locator(&self) -> Option<Vec<block::Hash>> {
        let tip_height = self.best_tip()?.0;

        let heights = crate::util::block_locator_heights(tip_height);
        let mut hashes = Vec::with_capacity(heights.len());

        for height in heights {
            if let Some(hash) = self.best_hash(height) {
                hashes.push(hash);
            }
        }

        Some(hashes)
    }

    /// Return the tip of the current best chain.
    pub fn best_tip(&self) -> Option<(block::Height, block::Hash)> {
        self.mem.best_tip().or_else(|| self.disk.tip())
    }

    /// Return the depth of block `hash` in the current best chain.
    pub fn best_depth(&self, hash: block::Hash) -> Option<u32> {
        let tip = self.best_tip()?.0;
        let height = self
            .mem
            .best_height_by_hash(hash)
            .or_else(|| self.disk.height(hash))?;

        Some(tip.0 - height.0)
    }

    /// Return the block identified by either its `height` or `hash` if it exists
    /// in the current best chain.
    pub fn best_block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        self.mem
            .best_block(hash_or_height)
            .or_else(|| self.disk.block(hash_or_height))
    }

    /// Return the transaction identified by `hash` if it exists in the current
    /// best chain.
    pub fn best_transaction(&self, hash: transaction::Hash) -> Option<Arc<Transaction>> {
        self.mem
            .best_transaction(hash)
            .or_else(|| self.disk.transaction(hash))
    }

    /// Return the hash for the block at `height` in the current best chain.
    pub fn best_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.mem
            .best_hash(height)
            .or_else(|| self.disk.hash(height))
    }

    /// Return true if `hash` is in the current best chain.
    pub fn best_chain_contains(&self, hash: block::Hash) -> bool {
        self.best_height_by_hash(hash).is_some()
    }

    /// Return the height for the block at `hash`, if `hash` is in the best chain.
    pub fn best_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.mem
            .best_height_by_hash(hash)
            .or_else(|| self.disk.height(hash))
    }

    /// Return the height for the block at `hash` in any chain.
    pub fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.mem
            .any_height_by_hash(hash)
            .or_else(|| self.disk.height(hash))
    }

    /// Return the [`Utxo`] pointed to by `outpoint` if it exists in any chain.
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<Utxo> {
        self.mem
            .any_utxo(outpoint)
            .or_else(|| self.queued_blocks.utxo(outpoint))
            .or_else(|| self.disk.utxo(outpoint))
    }

    /// Return an iterator over the relevant chain of the block identified by
    /// `hash`, in order from the largest height to the genesis block.
    ///
    /// The block identified by `hash` is included in the chain of blocks yielded
    /// by the iterator. `hash` can come from any chain.
    pub fn any_ancestor_blocks(&self, hash: block::Hash) -> Iter<'_> {
        Iter {
            service: self,
            state: IterState::NonFinalized(hash),
        }
    }

    /// Find the first hash that's in the peer's `known_blocks` and the local best chain.
    ///
    /// Returns `None` if:
    ///   * there is no matching hash in the best chain, or
    ///   * the state is empty.
    fn find_best_chain_intersection(&self, known_blocks: Vec<block::Hash>) -> Option<block::Hash> {
        // We can get a block locator request before we have downloaded the genesis block
        self.best_tip()?;

        known_blocks
            .iter()
            .find(|&&hash| self.best_chain_contains(hash))
            .cloned()
    }

    /// Returns a list of block hashes in the best chain, following the `intersection` with the best
    /// chain. If there is no intersection with the best chain, starts from the genesis hash.
    ///
    /// Includes finalized and non-finalized blocks.
    ///
    /// Stops the list of hashes after:
    ///   * adding the best tip,
    ///   * adding the `stop` hash to the list, if it is in the best chain, or
    ///   * adding `max_len` hashes to the list.
    ///
    /// Returns an empty list if the state is empty.
    pub fn collect_best_chain_hashes(
        &self,
        intersection: Option<block::Hash>,
        stop: Option<block::Hash>,
        max_len: usize,
    ) -> Vec<block::Hash> {
        assert!(max_len > 0, "max_len must be at least 1");

        // We can get a block locator request before we have downloaded the genesis block
        let chain_tip_height = if let Some((height, _)) = self.best_tip() {
            height
        } else {
            return Vec::new();
        };

        let intersection_height = intersection.map(|hash| {
            self.best_height_by_hash(hash)
                .expect("the intersection hash must be in the best chain")
        });
        let max_len_height = if let Some(intersection_height) = intersection_height {
            // start after the intersection_height, and return max_len hashes
            (intersection_height + (max_len as i32))
                .expect("the Find response height does not exceed Height::MAX")
        } else {
            // start at genesis, and return max_len hashes
            block::Height((max_len - 1) as _)
        };

        let stop_height = stop.map(|hash| self.best_height_by_hash(hash)).flatten();

        // Compute the final height, making sure it is:
        //   * at or below our chain tip, and
        //   * at or below the height of the stop hash.
        let final_height = std::cmp::min(max_len_height, chain_tip_height);
        let final_height = stop_height
            .map(|stop_height| std::cmp::min(final_height, stop_height))
            .unwrap_or(final_height);
        let final_hash = self
            .best_hash(final_height)
            .expect("final height must have a hash");

        // We can use an "any chain" method here, because `final_hash` is in the best chain
        let mut res: Vec<_> = self
            .any_ancestor_blocks(final_hash)
            .map(|block| block.hash())
            .take_while(|&hash| Some(hash) != intersection)
            .inspect(|hash| {
                tracing::trace!(
                    ?hash,
                    height = ?self.best_height_by_hash(*hash)
                        .expect("if hash is in the state then it should have an associated height"),
                    "adding hash to peer Find response",
                )
            })
            .collect();
        res.reverse();

        tracing::info!(
            ?final_height,
            response_len = ?res.len(),
            ?chain_tip_height,
            ?stop_height,
            ?intersection_height,
            "responding to peer GetBlocks or GetHeaders",
        );

        // Check the function implements the Find protocol
        assert!(
            res.len() <= max_len,
            "a Find response must not exceed the maximum response length"
        );
        assert!(
            intersection
                .map(|hash| !res.contains(&hash))
                .unwrap_or(true),
            "the list must not contain the intersection hash"
        );
        assert!(
            stop.map(|hash| !res[..(res.len() - 1)].contains(&hash))
                .unwrap_or(true),
            "if the stop hash is in the list, it must be the final hash"
        );

        res
    }

    /// Finds the first hash that's in the peer's `known_blocks` and the local best chain.
    /// Returns a list of hashes that follow that intersection, from the best chain.
    ///
    /// Starts from the first matching hash in the best chain, ignoring all other hashes in
    /// `known_blocks`. If there is no matching hash in the best chain, starts from the genesis
    /// hash.
    ///
    /// Includes finalized and non-finalized blocks.
    ///
    /// Stops the list of hashes after:
    ///   * adding the best tip,
    ///   * adding the `stop` hash to the list, if it is in the best chain, or
    ///   * adding 500 hashes to the list.
    ///
    /// Returns an empty list if the state is empty.
    pub fn find_best_chain_hashes(
        &self,
        known_blocks: Vec<block::Hash>,
        stop: Option<block::Hash>,
        max_len: usize,
    ) -> Vec<block::Hash> {
        let intersection = self.find_best_chain_intersection(known_blocks);
        self.collect_best_chain_hashes(intersection, stop, max_len)
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

        if let Some(block) = service.mem.any_block_by_hash(hash) {
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
                .any_height_by_hash(hash)
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
            let tip = self.best_tip();
            let old_len = self.pending_utxos.len();

            self.pending_utxos.prune();
            self.last_prune = now;

            let new_len = self.pending_utxos.len();
            let prune_count = old_len
                .checked_sub(new_len)
                .expect("prune does not add any utxo requests");
            if prune_count > 0 {
                tracing::info!(
                    ?old_len,
                    ?new_len,
                    ?prune_count,
                    ?tip,
                    "pruned utxo requests"
                );
            } else {
                tracing::debug!(len = ?old_len, ?tip, "no utxo requests needed pruning");
            }
        }

        Poll::Ready(Ok(()))
    }

    #[instrument(name = "state", skip(self, req))]
    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::CommitBlock(prepared) => {
                metrics::counter!("state.requests", 1, "type" => "commit_block");

                self.pending_utxos.check_against(&prepared.new_outputs);
                let rsp_rx = self.queue_and_commit_non_finalized(prepared);

                async move {
                    rsp_rx
                        .await
                        .expect("sender is not dropped")
                        .map(Response::Committed)
                        .map_err(Into::into)
                }
                .boxed()
            }
            Request::CommitFinalizedBlock(finalized) => {
                metrics::counter!("state.requests", 1, "type" => "commit_finalized_block");

                let (rsp_tx, rsp_rx) = oneshot::channel();

                self.pending_utxos.check_against(&finalized.new_outputs);
                self.disk.queue_and_commit_finalized((finalized, rsp_tx));

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
                let rsp = Ok(self.best_depth(hash)).map(Response::Depth);
                async move { rsp }.boxed()
            }
            Request::Tip => {
                metrics::counter!("state.requests", 1, "type" => "tip");
                let rsp = Ok(self.best_tip()).map(Response::Tip);
                async move { rsp }.boxed()
            }
            Request::BlockLocator => {
                metrics::counter!("state.requests", 1, "type" => "block_locator");
                let rsp = Ok(self.block_locator().unwrap_or_default()).map(Response::BlockLocator);
                async move { rsp }.boxed()
            }
            Request::Transaction(hash) => {
                metrics::counter!("state.requests", 1, "type" => "transaction");
                let rsp = Ok(self.best_transaction(hash)).map(Response::Transaction);
                async move { rsp }.boxed()
            }
            Request::Block(hash_or_height) => {
                metrics::counter!("state.requests", 1, "type" => "block");
                let rsp = Ok(self.best_block(hash_or_height)).map(Response::Block);
                async move { rsp }.boxed()
            }
            Request::AwaitUtxo(outpoint) => {
                metrics::counter!("state.requests", 1, "type" => "await_utxo");

                let fut = self.pending_utxos.queue(outpoint);

                if let Some(utxo) = self.any_utxo(&outpoint) {
                    self.pending_utxos.respond(&outpoint, utxo);
                }

                fut.boxed()
            }
            Request::FindBlockHashes { known_blocks, stop } => {
                const MAX_FIND_BLOCK_HASHES_RESULTS: usize = 500;
                let res =
                    self.find_best_chain_hashes(known_blocks, stop, MAX_FIND_BLOCK_HASHES_RESULTS);
                async move { Ok(Response::BlockHashes(res)) }.boxed()
            }
            Request::FindBlockHeaders { known_blocks, stop } => {
                const MAX_FIND_BLOCK_HEADERS_RESULTS: usize = 160;
                // Zcashd will blindly request more block headers as long as it
                // got 160 block headers in response to a previous query, EVEN
                // IF THOSE HEADERS ARE ALREADY KNOWN.  To dodge this behavior,
                // return slightly fewer than the maximum, to get it to go away.
                //
                // https://github.com/bitcoin/bitcoin/pull/4468/files#r17026905
                let count = MAX_FIND_BLOCK_HEADERS_RESULTS - 2;
                let res = self.find_best_chain_hashes(known_blocks, stop, count);
                let res: Vec<_> = res
                    .iter()
                    .map(|&hash| {
                        let block = self
                            .best_block(hash.into())
                            .expect("block for found hash is in the best chain");
                        block::CountedHeader {
                            transaction_count: block.transactions.len(),
                            header: block.header,
                        }
                    })
                    .collect();
                async move { Ok(Response::BlockHeaders(res)) }.boxed()
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

/// Check if zebra is following a legacy chain and return an error if so.
fn legacy_chain_check<I>(
    nu5_activation_height: block::Height,
    ancestors: I,
    network: Network,
) -> Result<(), BoxError>
where
    I: Iterator<Item = Arc<Block>>,
{
    const MAX_BLOCKS_TO_CHECK: usize = 100;

    for (count, block) in ancestors.enumerate() {
        // Stop checking if the chain reaches Canopy. We won't find any more V5 transactions,
        // so the rest of our checks are useless.
        //
        // If the cached tip is close to NU5 activation, but there aren't any V5 transactions in the
        // chain yet, we could reach MAX_BLOCKS_TO_CHECK in Canopy, and incorrectly return an error.
        if block
            .coinbase_height()
            .expect("valid blocks have coinbase heights")
            < nu5_activation_height
        {
            return Ok(());
        }

        // If we are past our NU5 activation height, but there are no V5 transactions in recent blocks,
        // the Zebra instance that verified those blocks had no NU5 activation height.
        if count >= MAX_BLOCKS_TO_CHECK {
            return Err("giving up after checking too many blocks".into());
        }

        // If a transaction `network_upgrade` field is different from the network upgrade calculated
        // using our activation heights, the Zebra instance that verified those blocks had different
        // network upgrade heights.
        block
            .check_transaction_network_upgrade_consistency(network)
            .map_err(|_| "inconsistent network upgrade found in transaction")?;

        // If we find at least one transaction with a valid `network_upgrade` field, the Zebra instance that
        // verified those blocks used the same network upgrade heights. (Up to this point in the chain.)
        let has_network_upgrade = block
            .transactions
            .iter()
            .find_map(|trans| trans.network_upgrade())
            .is_some();
        if has_network_upgrade {
            return Ok(());
        }
    }

    Ok(())
}
