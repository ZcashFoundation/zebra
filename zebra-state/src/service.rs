//! [`tower::Service`]s for Zebra's cached chain state.
//!
//! Zebra provides cached state access via two main services:
//! - [`StateService`]: a read-write service that waits for queued blocks.
//! - [`ReadStateService`]: a read-only service that answers from the most
//!   recent committed block.
//!
//! Most users should prefer [`ReadStateService`], unless they need to wait for
//! verified blocks to be committed. (For example, the syncer and mempool
//! tasks.)
//!
//! Zebra also provides access to the best chain tip via:
//! - [`LatestChainTip`]: a read-only channel that contains the latest committed
//!   tip.
//! - [`ChainTipChange`]: a read-only channel that can asynchronously await
//!   chain tip changes.

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
use tracing::{instrument, Instrument, Span};

#[cfg(any(test, feature = "proptest-impl"))]
use tower::buffer::Buffer;

use zebra_chain::{
    block::{self, CountedHeader},
    diagnostic::CodeTimer,
    parameters::{Network, NetworkUpgrade},
    transparent,
};

use crate::{
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
/// using the [`ReadStateService`].
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
        let timer = CodeTimer::start();
        let disk = FinalizedState::new(&config, network);
        timer.finish(module_path!(), line!(), "opening finalized state database");

        let timer = CodeTimer::start();
        let initial_tip = disk
            .db()
            .tip_block()
            .map(FinalizedBlock::from)
            .map(ChainTipBlock::from);
        timer.finish(module_path!(), line!(), "fetching database tip");

        let timer = CodeTimer::start();
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
        timer.finish(module_path!(), line!(), "initializing state service");

        tracing::info!("starting legacy chain check");
        let timer = CodeTimer::start();

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
        timer.finish(module_path!(), line!(), "legacy chain check");

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
                tip_block_height.0 as f64,
            );

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because the state makes sure that blocks
            // are committed in order.
            metrics::gauge!(
                "zcash.chain.verified.block.height",
                tip_block_height.0 as f64,
            );
        }

        tracing::trace!("finished processing queued block");
        rsp_rx
    }

    /// Update the [`LatestChainTip`], [`ChainTipChange`], and `best_chain_sender`
    /// channels with the latest non-finalized [`ChainTipBlock`] and
    /// [`Chain`][1].
    ///
    /// Returns the latest non-finalized chain tip height, or `None` if the
    /// non-finalized state is empty.
    ///
    /// [1]: non_finalized_state::Chain
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

    /// Return the hash for the block at `height` in the current best chain.
    pub fn best_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.mem
            .best_hash(height)
            .or_else(|| self.disk.db().hash(height))
    }

    /// Return true if `hash` is in the current best chain.
    #[allow(dead_code)]
    pub fn best_chain_contains(&self, hash: block::Hash) -> bool {
        read::chain_contains_hash(self.mem.best_chain(), self.disk.db(), hash)
    }

    /// Return the height for the block at `hash`, if `hash` is in the best chain.
    #[allow(dead_code)]
    pub fn best_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        read::height_by_hash(self.mem.best_chain(), self.disk.db(), hash)
    }

    /// Return the height for the block at `hash` in any chain.
    pub fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.mem
            .any_height_by_hash(hash)
            .or_else(|| self.disk.db().height(hash))
    }

    /// Return the [`transparent::Utxo`] pointed to by `outpoint`, if it exists
    /// in any chain, or in any pending block.
    ///
    /// Some of the returned UTXOs may be invalid, because:
    /// - they are not in the best chain, or
    /// - their block fails contextual validation.
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        // We ignore any UTXOs in FinalizedState.queued_by_prev_hash,
        // because it is only used during checkpoint verification.
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

                let timer = CodeTimer::start();

                self.assert_block_can_be_validated(&prepared);

                self.pending_utxos
                    .check_against_ordered(&prepared.new_outputs);

                // # Performance
                //
                // Allow other async tasks to make progress while blocks are being verified
                // and written to disk. But wait for the blocks to finish committing,
                // so that `StateService` multi-block queries always observe a consistent state.
                //
                // Since each block is spawned into its own task,
                // there shouldn't be any other code running in the same task,
                // so we don't need to worry about blocking it:
                // https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html
                let span = Span::current();
                let rsp_rx = tokio::task::block_in_place(move || {
                    span.in_scope(|| self.queue_and_commit_non_finalized(prepared))
                });

                // The work is all done, the future just waits on a channel for the result
                timer.finish(module_path!(), line!(), "CommitBlock");

                let span = Span::current();
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
                .instrument(span)
                .boxed()
            }
            Request::CommitFinalizedBlock(finalized) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "commit_finalized_block",
                );

                let timer = CodeTimer::start();

                self.pending_utxos.check_against(&finalized.new_outputs);

                // # Performance
                //
                // Allow other async tasks to make progress while blocks are being verified
                // and written to disk.
                //
                // See the note in `CommitBlock` for more details.
                let span = Span::current();
                let rsp_rx = tokio::task::block_in_place(move || {
                    span.in_scope(|| self.queue_and_commit_finalized(finalized))
                });

                // The work is all done, the future just waits on a channel for the result
                timer.finish(module_path!(), line!(), "CommitFinalizedBlock");

                let span = Span::current();
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
                .instrument(span)
                .boxed()
            }
            Request::Depth(hash) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "depth",
                );

                let timer = CodeTimer::start();

                // TODO: move this work into the future, like Block and Transaction?
                let rsp = Ok(Response::Depth(self.best_depth(hash)));

                // The work is all done, the future just returns the result.
                timer.finish(module_path!(), line!(), "Depth");

                async move { rsp }.boxed()
            }
            // TODO: consider spawning small reads into blocking tasks,
            //       because the database can do large cleanups during small reads.
            Request::Tip => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "tip",
                );

                let timer = CodeTimer::start();

                // TODO: move this work into the future, like Block and Transaction?
                let rsp = Ok(Response::Tip(self.best_tip()));

                // The work is all done, the future just returns the result.
                timer.finish(module_path!(), line!(), "Tip");

                async move { rsp }.boxed()
            }
            Request::BlockLocator => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "block_locator",
                );

                let timer = CodeTimer::start();

                // TODO: move this work into the future, like Block and Transaction?
                let rsp = Ok(Response::BlockLocator(
                    self.block_locator().unwrap_or_default(),
                ));

                // The work is all done, the future just returns the result.
                timer.finish(module_path!(), line!(), "BlockLocator");

                async move { rsp }.boxed()
            }
            Request::Transaction(hash) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "transaction",
                );

                let timer = CodeTimer::start();

                // Prepare data for concurrent execution
                let best_chain = self.mem.best_chain().cloned();
                let db = self.disk.db().clone();

                // # Performance
                //
                // Allow other async tasks to make progress while the transaction is being read from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(|| {
                        let rsp = read::transaction(best_chain, &db, hash);

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "Transaction");

                        Ok(Response::Transaction(rsp.map(|(tx, _height)| tx)))
                    })
                })
                .map(|join_result| join_result.expect("panic in Request::Transaction"))
                .boxed()
            }
            Request::Block(hash_or_height) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "block",
                );

                let timer = CodeTimer::start();

                // Prepare data for concurrent execution
                let best_chain = self.mem.best_chain().cloned();
                let db = self.disk.db().clone();

                // # Performance
                //
                // Allow other async tasks to make progress while the block is being read from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let rsp = read::block(best_chain, &db, hash_or_height);

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "Block");

                        Ok(Response::Block(rsp))
                    })
                })
                .map(|join_result| join_result.expect("panic in Request::Block"))
                .boxed()
            }
            Request::AwaitUtxo(outpoint) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "await_utxo",
                );

                let timer = CodeTimer::start();
                let span = Span::current();

                let fut = self.pending_utxos.queue(outpoint);

                if let Some(utxo) = self.any_utxo(&outpoint) {
                    self.pending_utxos.respond(&outpoint, utxo);
                }

                // The future waits on a channel for a response.
                timer.finish(module_path!(), line!(), "AwaitUtxo");

                fut.instrument(span).boxed()
            }
            Request::FindBlockHashes { known_blocks, stop } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "find_block_hashes",
                );

                const MAX_FIND_BLOCK_HASHES_RESULTS: u32 = 500;

                let timer = CodeTimer::start();

                // Prepare data for concurrent execution
                let best_chain = self.mem.best_chain().cloned();
                let db = self.disk.db().clone();

                // # Performance
                //
                // Allow other async tasks to make progress while the block is being read from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let res = read::find_chain_hashes(
                            best_chain,
                            &db,
                            known_blocks,
                            stop,
                            MAX_FIND_BLOCK_HASHES_RESULTS,
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "FindBlockHashes");

                        Ok(Response::BlockHashes(res))
                    })
                })
                .map(|join_result| join_result.expect("panic in Request::Block"))
                .boxed()
            }
            Request::FindBlockHeaders { known_blocks, stop } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "find_block_headers",
                );

                // Before we spawn the future, get a consistent set of chain hashes from the state.

                const MAX_FIND_BLOCK_HEADERS_RESULTS: u32 = 160;
                // Zcashd will blindly request more block headers as long as it
                // got 160 block headers in response to a previous query, EVEN
                // IF THOSE HEADERS ARE ALREADY KNOWN.  To dodge this behavior,
                // return slightly fewer than the maximum, to get it to go away.
                //
                // https://github.com/bitcoin/bitcoin/pull/4468/files#r17026905
                let max_len = MAX_FIND_BLOCK_HEADERS_RESULTS - 2;

                let timer = CodeTimer::start();

                // Prepare data for concurrent execution
                let best_chain = self.mem.best_chain().cloned();
                let db = self.disk.db().clone();

                // # Performance
                //
                // Allow other async tasks to make progress while the block is being read from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let res =
                            read::find_chain_headers(best_chain, &db, known_blocks, stop, max_len);
                        let res = res
                            .into_iter()
                            .map(|header| CountedHeader { header })
                            .collect();

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "FindBlockHeaders");

                        Ok(Response::BlockHeaders(res))
                    })
                })
                .map(|join_result| join_result.expect("panic in Request::Block"))
                .boxed()
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
            // Used by get_block RPC.
            ReadRequest::Block(hash_or_height) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "block",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                // # Performance
                //
                // Allow other async tasks to make progress while concurrently reading blocks from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let block = state.best_chain_receiver.with_watch_data(|best_chain| {
                            read::block(best_chain, &state.db, hash_or_height)
                        });

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::Block");

                        Ok(ReadResponse::Block(block))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Block"))
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

                let timer = CodeTimer::start();

                let state = self.clone();

                // # Performance
                //
                // Allow other async tasks to make progress while concurrently reading transactions from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let transaction_and_height =
                            state.best_chain_receiver.with_watch_data(|best_chain| {
                                read::transaction(best_chain, &state.db, hash)
                            });

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::Transaction");

                        Ok(ReadResponse::Transaction(transaction_and_height))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Transaction"))
                .boxed()
            }

            ReadRequest::SaplingTree(hash_or_height) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "sapling_tree",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                // # Performance
                //
                // Allow other async tasks to make progress while concurrently reading trees from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let sapling_tree =
                            state.best_chain_receiver.with_watch_data(|best_chain| {
                                read::sapling_tree(best_chain, &state.db, hash_or_height)
                            });

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::SaplingTree");

                        Ok(ReadResponse::SaplingTree(sapling_tree))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::SaplingTree"))
                .boxed()
            }

            ReadRequest::OrchardTree(hash_or_height) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "orchard_tree",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                // # Performance
                //
                // Allow other async tasks to make progress while concurrently reading trees from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let orchard_tree =
                            state.best_chain_receiver.with_watch_data(|best_chain| {
                                read::orchard_tree(best_chain, &state.db, hash_or_height)
                            });

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::OrchardTree");

                        Ok(ReadResponse::OrchardTree(orchard_tree))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::OrchardTree"))
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

                let timer = CodeTimer::start();

                let state = self.clone();

                // # Performance
                //
                // Allow other async tasks to make progress while concurrently reading transaction IDs from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let tx_ids = state.best_chain_receiver.with_watch_data(|best_chain| {
                            read::transparent_tx_ids(best_chain, &state.db, addresses, height_range)
                        });

                        // The work is done in the future.
                        timer.finish(
                            module_path!(),
                            line!(),
                            "ReadRequest::TransactionIdsByAddresses",
                        );

                        tx_ids.map(ReadResponse::AddressesTransactionIds)
                    })
                })
                .map(|join_result| {
                    join_result.expect("panic in ReadRequest::TransactionIdsByAddresses")
                })
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

                let timer = CodeTimer::start();

                let state = self.clone();

                // # Performance
                //
                // Allow other async tasks to make progress while concurrently reading balances from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let balance = state.best_chain_receiver.with_watch_data(|best_chain| {
                            read::transparent_balance(best_chain, &state.db, addresses)
                        })?;

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::AddressBalance");

                        Ok(ReadResponse::AddressBalance(balance))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::AddressBalance"))
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

                let timer = CodeTimer::start();

                let state = self.clone();

                // # Performance
                //
                // Allow other async tasks to make progress while concurrently reading UTXOs from disk.
                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let utxos = state.best_chain_receiver.with_watch_data(|best_chain| {
                            read::transparent_utxos(state.network, best_chain, &state.db, addresses)
                        });

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::UtxosByAddresses");

                        utxos.map(ReadResponse::Utxos)
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::UtxosByAddresses"))
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
