//! [`tower::Service`]s for Zebra's cached chain state.
//!
//! Zebra provides cached state access via two main services:
//! - [`StateService`]: a read-write service that waits for queued blocks.
//! - [`ReadStateService`]: a read-only service that answers from the most recent committed block.
//!
//! Most users should prefer [`ReadStateService`], unless they need to wait for
//! verified blocks to be committed. (For example, the syncer and mempool tasks.)
//!
//! Zebra also provides access to the best chain tip via:
//! - [`LatestChainTip`]: a read-only channel that contains the latest committed tip.
//! - [`ChainTipChange`]: a read-only channel that can asynchronously await chain tip changes.

use std::{
    convert,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures::future::FutureExt;
use tokio::sync::{oneshot, watch};
use tower::{util::BoxService, Service};
use tracing::instrument;

#[cfg(any(test, feature = "proptest-impl"))]
use tower::buffer::Buffer;

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade},
    transaction,
    transaction::Transaction,
    transparent,
};

use crate::{
    request::HashOrHeight,
    service::{
        chain_tip::{ChainTipBlock, ChainTipChange, ChainTipSender, LatestChainTip},
        finalized_state::{FinalizedState, ZebraDb},
        non_finalized_state::{Chain, NonFinalizedState, QueuedBlocks},
        pending_utxos::PendingUtxos,
        watch_receiver::WatchReceiver,
    },
    BoxError, CloneError, CommitBlockError, Config, FinalizedBlock, PreparedBlock, ReadRequest,
    ReadResponse, Request, Response, ValidateContextError,
};

pub mod block_iter;
pub mod chain_tip;
pub mod watch_receiver;

pub(crate) mod check;

mod finalized_state;
mod non_finalized_state;
mod pending_utxos;
pub(crate) mod read;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

#[cfg(test)]
mod tests;

pub use finalized_state::{OutputIndex, OutputLocation, TransactionLocation};

pub type QueuedBlock = (
    PreparedBlock,
    oneshot::Sender<Result<block::Hash, BoxError>>,
);
pub type QueuedFinalized = (
    FinalizedBlock,
    oneshot::Sender<Result<block::Hash, BoxError>>,
);

/// A read-write service for Zebra's cached blockchain state.
///
/// This service modifies and provides access to:
/// - the non-finalized state: the ~100 most recent blocks.
///                            Zebra allows chain forks in the non-finalized state,
///                            stores it in memory, and re-downloads it when restarted.
/// - the finalized state: older blocks that have many confirmations.
///                        Zebra stores the single best chain in the finalized state,
///                        and re-loads it from disk when restarted.
///
/// Requests to this service are processed in series,
/// so read requests wait for all queued write requests to complete,
/// then return their answers.
///
/// This behaviour is implicitly used by Zebra's syncer,
/// to delay the next ObtainTips until all queued blocks have been committed.
///
/// But most state users can ignore any queued blocks, and get faster read responses
/// using the [`ReadOnlyStateService`].
#[derive(Debug)]
pub(crate) struct StateService {
    /// The finalized chain state, including its on-disk database.
    pub(crate) disk: FinalizedState,

    /// The non-finalized chain state, including its in-memory chain forks.
    mem: NonFinalizedState,

    /// The configured Zcash network.
    network: Network,

    /// Blocks awaiting their parent blocks for contextual verification.
    queued_blocks: QueuedBlocks,

    /// The set of outpoints with pending requests for their associated transparent::Output.
    pending_utxos: PendingUtxos,

    /// Instant tracking the last time `pending_utxos` was pruned.
    last_prune: Instant,

    /// A sender channel for the current best chain tip.
    chain_tip_sender: ChainTipSender,

    /// A sender channel for the current best non-finalized chain.
    best_chain_sender: watch::Sender<Option<Arc<Chain>>>,
}

/// A read-only service for accessing Zebra's cached blockchain state.
///
/// This service provides read-only access to:
/// - the non-finalized state: the ~100 most recent blocks.
/// - the finalized state: older blocks that have many confirmations.
///
/// Requests to this service are processed in parallel,
/// ignoring any blocks queued by the read-write [`StateService`].
///
/// This quick response behavior is better for most state users.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct ReadStateService {
    /// The shared inner on-disk database for the finalized state.
    ///
    /// RocksDB allows reads and writes via a shared reference,
    /// but [`ZebraDb`] doesn't expose any write methods or types.
    ///
    /// This chain is updated concurrently with requests,
    /// so it might include some block data that is also in `best_mem`.
    db: ZebraDb,

    /// A watch channel for the current best in-memory chain.
    ///
    /// This chain is only updated between requests,
    /// so it might include some block data that is also on `disk`.
    best_chain_receiver: WatchReceiver<Option<Arc<Chain>>>,

    /// The configured Zcash network.
    network: Network,
}

impl StateService {
    const PRUNE_INTERVAL: Duration = Duration::from_secs(30);

    /// Create a new read-write state service.
    /// Returns the read-write and read-only state services,
    /// and read-only watch channels for its best chain tip.
    pub fn new(
        config: Config,
        network: Network,
    ) -> (Self, ReadStateService, LatestChainTip, ChainTipChange) {
        let disk = FinalizedState::new(&config, network);
        let initial_tip = disk
            .db()
            .tip_block()
            .map(FinalizedBlock::from)
            .map(ChainTipBlock::from);
        let (chain_tip_sender, latest_chain_tip, chain_tip_change) =
            ChainTipSender::new(initial_tip, network);

        let mem = NonFinalizedState::new(network);

        let (read_only_service, best_chain_sender) = ReadStateService::new(&disk);

        let queued_blocks = QueuedBlocks::default();
        let pending_utxos = PendingUtxos::default();

        let state = Self {
            disk,
            mem,
            queued_blocks,
            pending_utxos,
            network,
            last_prune: Instant::now(),
            chain_tip_sender,
            best_chain_sender,
        };

        tracing::info!("starting legacy chain check");
        if let Some(tip) = state.best_tip() {
            if let Some(nu5_activation_height) = NetworkUpgrade::Nu5.activation_height(network) {
                if check::legacy_chain(
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

        (state, read_only_service, latest_chain_tip, chain_tip_change)
    }

    /// Queue a finalized block for verification and storage in the finalized state.
    fn queue_and_commit_finalized(
        &mut self,
        finalized: FinalizedBlock,
    ) -> oneshot::Receiver<Result<block::Hash, BoxError>> {
        let (rsp_tx, rsp_rx) = oneshot::channel();

        let tip_block = self
            .disk
            .queue_and_commit_finalized((finalized, rsp_tx))
            .map(ChainTipBlock::from);
        self.chain_tip_sender.set_finalized_tip(tip_block);

        rsp_rx
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

        if self.mem.any_chain_contains(&prepared.hash)
            || self.disk.db().hash(prepared.height).is_some()
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
                .expect(
                    "expected that errors would not occur when writing to disk or updating note commitment and history trees",
                );
        }

        let finalized_tip_height = self.disk.db().finalized_tip_height().expect(
            "Finalized state must have at least one block before committing non-finalized state",
        );
        self.queued_blocks.prune_by_height(finalized_tip_height);

        let tip_block_height = self.update_latest_chain_channels();

        // update metrics using the best non-finalized tip
        if let Some(tip_block_height) = tip_block_height {
            metrics::gauge!(
                "state.full_verifier.committed.block.height",
                tip_block_height.0 as _
            );

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because the state makes sure that blocks
            // are committed in order.
            metrics::gauge!("zcash.chain.verified.block.height", tip_block_height.0 as _);
        }

        tracing::trace!("finished processing queued block");
        rsp_rx
    }

    /// Update the [`LatestChainTip`], [`ChainTipChange`], and [`LatestChain`] channels
    /// with the latest non-finalized [`ChainTipBlock`] and [`Chain`].
    ///
    /// Returns the latest non-finalized chain tip height,
    /// or `None` if the non-finalized state is empty.
    #[instrument(level = "debug", skip(self))]
    fn update_latest_chain_channels(&mut self) -> Option<block::Height> {
        let best_chain = self.mem.best_chain();
        let tip_block = best_chain
            .and_then(|chain| chain.tip_block())
            .cloned()
            .map(ChainTipBlock::from);
        let tip_block_height = tip_block.as_ref().map(|block| block.height);

        // The RPC service uses the ReadStateService, but it is not turned on by default.
        if self.best_chain_sender.receiver_count() > 0 {
            // If the final receiver was just dropped, ignore the error.
            let _ = self.best_chain_sender.send(best_chain.cloned());
        }

        self.chain_tip_sender.set_best_non_finalized_tip(tip_block);

        tip_block_height
    }

    /// Run contextual validation on the prepared block and add it to the
    /// non-finalized state if it is contextually valid.
    #[tracing::instrument(level = "debug", skip(self, prepared))]
    fn validate_and_commit(&mut self, prepared: PreparedBlock) -> Result<(), CommitBlockError> {
        self.check_contextual_validity(&prepared)?;
        let parent_hash = prepared.block.header.previous_block_hash;

        if self.disk.db().finalized_tip_hash() == parent_hash {
            self.mem.commit_new_chain(prepared, self.disk.db())?;
        } else {
            self.mem.commit_block(prepared, self.disk.db())?;
        }

        Ok(())
    }

    /// Returns `true` if `hash` is a valid previous block hash for new non-finalized blocks.
    fn can_fork_chain_at(&self, hash: &block::Hash) -> bool {
        self.mem.any_chain_contains(hash) || &self.disk.db().finalized_tip_hash() == hash
    }

    /// Attempt to validate and commit all queued blocks whose parents have
    /// recently arrived starting from `new_parent`, in breadth-first ordering.
    #[tracing::instrument(level = "debug", skip(self, new_parent))]
    fn process_queued(&mut self, new_parent: block::Hash) {
        let mut new_parents: Vec<(block::Hash, Result<(), CloneError>)> =
            vec![(new_parent, Ok(()))];

        while let Some((parent_hash, parent_result)) = new_parents.pop() {
            let queued_children = self.queued_blocks.dequeue_children(parent_hash);

            for (child, rsp_tx) in queued_children {
                let child_hash = child.hash;
                let result;

                // If the block is invalid, reject any descendant blocks.
                //
                // At this point, we know that the block and all its descendants
                // are invalid, because we checked all the consensus rules before
                // committing the block to the non-finalized state.
                // (These checks also bind the transaction data to the block
                // header, using the transaction merkle tree and authorizing data
                // commitment.)
                if let Err(ref parent_error) = parent_result {
                    tracing::trace!(
                        ?child_hash,
                        ?parent_error,
                        "rejecting queued child due to parent error"
                    );
                    result = Err(parent_error.clone());
                } else {
                    tracing::trace!(?child_hash, "validating queued child");
                    result = self.validate_and_commit(child).map_err(CloneError::from);
                    if result.is_ok() {
                        // Update the metrics if semantic and contextual validation passes
                        metrics::counter!("state.full_verifier.committed.block.count", 1);
                        metrics::counter!("zcash.chain.verified.block.total", 1);
                    }
                }

                let _ = rsp_tx.send(result.clone().map(|()| child_hash).map_err(BoxError::from));
                new_parents.push((child_hash, result));
            }
        }
    }

    /// Check that the prepared block is contextually valid for the configured
    /// network, based on the committed finalized and non-finalized state.
    ///
    /// Note: some additional contextual validity checks are performed by the
    /// non-finalized [`Chain`].
    fn check_contextual_validity(
        &mut self,
        prepared: &PreparedBlock,
    ) -> Result<(), ValidateContextError> {
        let relevant_chain = self.any_ancestor_blocks(prepared.block.header.previous_block_hash);

        // Security: check proof of work before any other checks
        check::block_is_valid_for_recent_chain(
            prepared,
            self.network,
            self.disk.db().finalized_tip_height(),
            relevant_chain,
        )?;

        check::nullifier::no_duplicates_in_finalized_chain(prepared, self.disk.db())?;

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
        self.mem.best_tip().or_else(|| self.disk.db().tip())
    }

    /// Return the depth of block `hash` in the current best chain.
    pub fn best_depth(&self, hash: block::Hash) -> Option<u32> {
        let tip = self.best_tip()?.0;
        let height = self
            .mem
            .best_height_by_hash(hash)
            .or_else(|| self.disk.db().height(hash))?;

        Some(tip.0 - height.0)
    }

    /// Returns the [`Block`] with [`Hash`](zebra_chain::block::Hash) or
    /// [`Height`](zebra_chain::block::Height), if it exists in the current best chain.
    pub fn best_block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        read::block(self.mem.best_chain(), self.disk.db(), hash_or_height)
    }

    /// Returns the [`Transaction`] with [`transaction::Hash`],
    /// if it exists in the current best chain.
    pub fn best_transaction(&self, hash: transaction::Hash) -> Option<Arc<Transaction>> {
        read::transaction(self.mem.best_chain(), self.disk.db(), hash).map(|(tx, _height)| tx)
    }

    /// Return the hash for the block at `height` in the current best chain.
    pub fn best_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.mem
            .best_hash(height)
            .or_else(|| self.disk.db().hash(height))
    }

    /// Return true if `hash` is in the current best chain.
    pub fn best_chain_contains(&self, hash: block::Hash) -> bool {
        self.best_height_by_hash(hash).is_some()
    }

    /// Return the height for the block at `hash`, if `hash` is in the best chain.
    pub fn best_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.mem
            .best_height_by_hash(hash)
            .or_else(|| self.disk.db().height(hash))
    }

    /// Return the height for the block at `hash` in any chain.
    pub fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.mem
            .any_height_by_hash(hash)
            .or_else(|| self.disk.db().height(hash))
    }

    /// Return the [`Utxo`] pointed to by `outpoint`, if it exists in any chain,
    /// or in any pending block.
    ///
    /// Some of the returned UTXOs may be invalid, because:
    /// - they are not in the best chain, or
    /// - their block fails contextual validation.
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        self.mem
            .any_utxo(outpoint)
            .or_else(|| self.queued_blocks.utxo(outpoint))
            .or_else(|| {
                self.disk
                    .db()
                    .utxo(outpoint)
                    .map(|ordered_utxo| ordered_utxo.utxo)
            })
    }

    /// Return an iterator over the relevant chain of the block identified by
    /// `hash`, in order from the largest height to the genesis block.
    ///
    /// The block identified by `hash` is included in the chain of blocks yielded
    /// by the iterator. `hash` can come from any chain.
    pub fn any_ancestor_blocks(&self, hash: block::Hash) -> block_iter::Iter<'_> {
        block_iter::Iter {
            service: self,
            state: block_iter::IterState::NonFinalized(hash),
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
    /// Returns an empty list if the state is empty,
    /// or if the `intersection` is the best chain tip.
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
            tracing::debug!(
                response_len = ?0,
                "responding to peer GetBlocks or GetHeaders with empty state",
            );

            return Vec::new();
        };

        let intersection_height = intersection.map(|hash| {
            self.best_height_by_hash(hash)
                .expect("the intersection hash must be in the best chain")
        });
        let max_len_height = if let Some(intersection_height) = intersection_height {
            let max_len = i32::try_from(max_len).expect("max_len fits in i32");

            // start after the intersection_height, and return max_len hashes
            (intersection_height + max_len)
                .expect("the Find response height does not exceed Height::MAX")
        } else {
            let max_len = u32::try_from(max_len).expect("max_len fits in u32");
            let max_height = block::Height(max_len);

            // start at genesis, and return max_len hashes
            (max_height - 1).expect("max_len is at least 1, and does not exceed Height::MAX + 1")
        };

        let stop_height = stop.and_then(|hash| self.best_height_by_hash(hash));

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

        tracing::debug!(
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
        if let (Some(stop), Some((_, res_except_last))) = (stop, res.split_last()) {
            assert!(
                !res_except_last.contains(&stop),
                "if the stop hash is in the list, it must be the final hash"
            );
        }

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

    /// Assert some assumptions about the prepared `block` before it is validated.
    fn assert_block_can_be_validated(&self, block: &PreparedBlock) {
        // required by validate_and_commit, moved here to make testing easier
        assert!(
            block.height > self.network.mandatory_checkpoint_height(),
            "invalid non-finalized block height: the canopy checkpoint is mandatory, pre-canopy \
            blocks, and the canopy activation block, must be committed to the state as finalized \
            blocks"
        );
    }
}

impl ReadStateService {
    /// Creates a new read-only state service, using the provided finalized state.
    ///
    /// Returns the newly created service,
    /// and a watch channel for updating its best non-finalized chain.
    pub(crate) fn new(disk: &FinalizedState) -> (Self, watch::Sender<Option<Arc<Chain>>>) {
        let (best_chain_sender, best_chain_receiver) = watch::channel(None);

        let read_only_service = Self {
            db: disk.db().clone(),
            best_chain_receiver: WatchReceiver::new(best_chain_receiver),
            network: disk.network(),
        };

        tracing::info!("created new read-only state service");

        (read_only_service, best_chain_sender)
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
                tracing::debug!(
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
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "commit_block",
                );

                self.assert_block_can_be_validated(&prepared);

                self.pending_utxos
                    .check_against_ordered(&prepared.new_outputs);
                let rsp_rx = self.queue_and_commit_non_finalized(prepared);

                async move {
                    rsp_rx
                        .await
                        .map_err(|_recv_error| {
                            BoxError::from("block was dropped from the state CommitBlock queue")
                        })
                        // TODO: replace with Result::flatten once it stabilises
                        // https://github.com/rust-lang/rust/issues/70142
                        .and_then(convert::identity)
                        .map(Response::Committed)
                        .map_err(Into::into)
                }
                .boxed()
            }
            Request::CommitFinalizedBlock(finalized) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "commit_finalized_block",
                );

                self.pending_utxos.check_against(&finalized.new_outputs);
                let rsp_rx = self.queue_and_commit_finalized(finalized);

                async move {
                    rsp_rx
                        .await
                        .map_err(|_recv_error| {
                            BoxError::from(
                                "block was dropped from the state CommitFinalizedBlock queue",
                            )
                        })
                        // TODO: replace with Result::flatten once it stabilises
                        // https://github.com/rust-lang/rust/issues/70142
                        .and_then(convert::identity)
                        .map(Response::Committed)
                        .map_err(Into::into)
                }
                .boxed()
            }
            Request::Depth(hash) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "depth",
                );

                let rsp = Ok(self.best_depth(hash)).map(Response::Depth);
                async move { rsp }.boxed()
            }
            Request::Tip => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "tip",
                );

                let rsp = Ok(self.best_tip()).map(Response::Tip);
                async move { rsp }.boxed()
            }
            Request::BlockLocator => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "block_locator",
                );

                let rsp = Ok(self.block_locator().unwrap_or_default()).map(Response::BlockLocator);
                async move { rsp }.boxed()
            }
            Request::Transaction(hash) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "transaction",
                );

                let rsp = Ok(self.best_transaction(hash)).map(Response::Transaction);
                async move { rsp }.boxed()
            }
            Request::Block(hash_or_height) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "block",
                );

                let rsp = Ok(self.best_block(hash_or_height)).map(Response::Block);
                async move { rsp }.boxed()
            }
            Request::AwaitUtxo(outpoint) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "await_utxo",
                );

                let fut = self.pending_utxos.queue(outpoint);

                if let Some(utxo) = self.any_utxo(&outpoint) {
                    self.pending_utxos.respond(&outpoint, utxo);
                }

                fut.boxed()
            }
            Request::FindBlockHashes { known_blocks, stop } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "find_block_hashes",
                );

                const MAX_FIND_BLOCK_HASHES_RESULTS: usize = 500;
                let res =
                    self.find_best_chain_hashes(known_blocks, stop, MAX_FIND_BLOCK_HASHES_RESULTS);
                async move { Ok(Response::BlockHashes(res)) }.boxed()
            }
            Request::FindBlockHeaders { known_blocks, stop } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "find_block_headers",
                );

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
                            transaction_count: block
                                .transactions
                                .len()
                                .try_into()
                                .expect("transaction count has already been validated"),
                            header: block.header,
                        }
                    })
                    .collect();
                async move { Ok(Response::BlockHeaders(res)) }.boxed()
            }
        }
    }
}

impl Service<ReadRequest> for ReadStateService {
    type Response = ReadResponse;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[instrument(name = "read_state", skip(self))]
    fn call(&mut self, req: ReadRequest) -> Self::Future {
        match req {
            // TODO: implement these new ReadRequests for lightwalletd, as part of these tickets

            // z_get_tree_state (#3156)

            // depends on transparent address indexes (#3150)
            // get_address_tx_ids (#3147)
            // get_address_balance (#3157)
            // get_address_utxos (#3158)

            // Used by get_block RPC.
            ReadRequest::Block(hash_or_height) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "block",
                );

                let state = self.clone();

                async move {
                    let block = state.best_chain_receiver.with_watch_data(|best_chain| {
                        read::block(best_chain, &state.db, hash_or_height)
                    });

                    Ok(ReadResponse::Block(block))
                }
                .boxed()
            }

            // For the get_raw_transaction RPC.
            ReadRequest::Transaction(hash) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "transaction",
                );

                let state = self.clone();

                async move {
                    let transaction_and_height =
                        state.best_chain_receiver.with_watch_data(|best_chain| {
                            read::transaction(best_chain, &state.db, hash)
                        });

                    Ok(ReadResponse::Transaction(transaction_and_height))
                }
                .boxed()
            }

            // For the get_address_tx_ids RPC.
            ReadRequest::TransactionIdsByAddresses {
                addresses,
                height_range,
            } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "transaction_ids_by_addresses",
                );

                let state = self.clone();

                async move {
                    let tx_ids = state.best_chain_receiver.with_watch_data(|best_chain| {
                        read::transparent_tx_ids(best_chain, &state.db, addresses, height_range)
                    });

                    tx_ids.map(ReadResponse::AddressesTransactionIds)
                }
                .boxed()
            }

            // For the get_address_balance RPC.
            ReadRequest::AddressBalance(addresses) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "address_balance",
                );

                let state = self.clone();

                async move {
                    let balance = state.best_chain_receiver.with_watch_data(|best_chain| {
                        read::transparent_balance(best_chain, &state.db, addresses)
                    })?;

                    Ok(ReadResponse::AddressBalance(balance))
                }
                .boxed()
            }

            // For the get_address_utxos RPC.
            ReadRequest::UtxosByAddresses(addresses) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "utxos_by_addresses",
                );

                let state = self.clone();

                async move {
                    let utxos = state.best_chain_receiver.with_watch_data(|best_chain| {
                        read::transparent_utxos(state.network, best_chain, &state.db, addresses)
                    });

                    utxos.map(ReadResponse::Utxos)
                }
                .boxed()
            }
        }
    }
}

/// Initialize a state service from the provided [`Config`].
/// Returns a boxed state service, a read-only state service,
/// and receivers for state chain tip updates.
///
/// Each `network` has its own separate on-disk database.
///
/// To share access to the state, wrap the returned service in a `Buffer`,
/// or clone the returned [`ReadStateService`].
///
/// It's possible to construct multiple state services in the same application (as
/// long as they, e.g., use different storage locations), but doing so is
/// probably not what you want.
pub fn init(
    config: Config,
    network: Network,
) -> (
    BoxService<Request, Response, BoxError>,
    ReadStateService,
    LatestChainTip,
    ChainTipChange,
) {
    let (state_service, read_only_state_service, latest_chain_tip, chain_tip_change) =
        StateService::new(config, network);

    (
        BoxService::new(state_service),
        read_only_state_service,
        latest_chain_tip,
        chain_tip_change,
    )
}

/// Returns a [`StateService`] with an ephemeral [`Config`] and a buffer with a single slot.
///
/// This can be used to create a state service for testing.
///
/// See also [`init`].
#[cfg(any(test, feature = "proptest-impl"))]
pub fn init_test(network: Network) -> Buffer<BoxService<Request, Response, BoxError>, Request> {
    let (state_service, _, _, _) = StateService::new(Config::ephemeral(), network);

    Buffer::new(BoxService::new(state_service), 1)
}

/// Initializes a state service with an ephemeral [`Config`] and a buffer with a single slot,
/// then returns the read-write service, read-only service, and tip watch channels.
///
/// This can be used to create a state service for testing. See also [`init`].
#[cfg(any(test, feature = "proptest-impl"))]
pub fn init_test_services(
    network: Network,
) -> (
    Buffer<BoxService<Request, Response, BoxError>, Request>,
    ReadStateService,
    LatestChainTip,
    ChainTipChange,
) {
    let (state_service, read_state_service, latest_chain_tip, chain_tip_change) =
        StateService::new(Config::ephemeral(), network);

    let state_service = Buffer::new(BoxService::new(state_service), 1);

    (
        state_service,
        read_state_service,
        latest_chain_tip,
        chain_tip_change,
    )
}
