//! [`tower::Service`]s for Zebra's cached chain state.
//!
//! Zebra provides cached state access via two main services:
//! - [`StateService`]: a read-write service that writes blocks to the state,
//!   and redirects most read requests to the [`ReadStateService`].
//! - [`ReadStateService`]: a read-only service that answers from the most
//!   recent committed block.
//!
//! Most users should prefer [`ReadStateService`], unless they need to write blocks to the state.
//!
//! Zebra also provides access to the best chain tip via:
//! - [`LatestChainTip`]: a read-only channel that contains the latest committed
//!   tip.
//! - [`ChainTipChange`]: a read-only channel that can asynchronously await
//!   chain tip changes.

use std::{
    collections::HashMap,
    convert,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures::future::FutureExt;
use tokio::sync::{oneshot, watch};
use tower::{util::BoxService, Service, ServiceExt};
use tracing::{instrument, Instrument, Span};

#[cfg(any(test, feature = "proptest-impl"))]
use tower::buffer::Buffer;

use zebra_chain::{
    block::{self, CountedHeader},
    diagnostic::CodeTimer,
    parameters::{Network, NetworkUpgrade},
};

use crate::{
    constants::{MAX_FIND_BLOCK_HASHES_RESULTS, MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_ZEBRA},
    service::{
        chain_tip::{ChainTipBlock, ChainTipChange, ChainTipSender, LatestChainTip},
        finalized_state::{FinalizedState, ZebraDb},
        non_finalized_state::NonFinalizedState,
        pending_utxos::PendingUtxos,
        queued_blocks::QueuedBlocks,
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
mod queued_blocks;
pub(crate) mod read;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

#[cfg(test)]
mod tests;

pub use finalized_state::{OutputIndex, OutputLocation, TransactionLocation};

use self::queued_blocks::QueuedFinalized;

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
/// Read requests to this service are buffered, then processed concurrently.
/// Block write requests are buffered, then queued, then processed in order by a separate task.
///
/// Most state users can get faster read responses using the [`ReadStateService`],
/// because its requests do not share a [`tower::buffer::Buffer`] with block write requests.
///
/// To quickly get the latest block, use [`LatestChainTip`] or [`ChainTipChange`].
/// They can read the latest block directly, without queueing any requests.
#[derive(Debug)]
pub(crate) struct StateService {
    // Configuration
    //
    /// The configured Zcash network.
    network: Network,

    // Queued Blocks
    //
    /// Queued blocks for the [`NonFinalizedState`] that arrived out of order.
    /// These blocks are awaiting their parent blocks before they can do contextual verification.
    queued_non_finalized_blocks: QueuedBlocks,

    /// Queued blocks for the [`FinalizedState`] that arrived out of order.
    /// These blocks are awaiting their parent blocks before they can do contextual verification.
    ///
    /// Indexed by their parent block hash.
    queued_finalized_blocks: HashMap<block::Hash, QueuedFinalized>,

    // Exclusively Writeable State
    //
    /// The non-finalized chain state, including its in-memory chain forks.
    mem: NonFinalizedState,

    /// The finalized chain state, including its on-disk database.
    pub(crate) disk: FinalizedState,

    // Pending UTXO Request Tracking
    //
    /// The set of outpoints with pending requests for their associated transparent::Output.
    pending_utxos: PendingUtxos,

    /// Instant tracking the last time `pending_utxos` was pruned.
    last_prune: Instant,

    // Concurrently Readable State
    //
    /// A sender channel used to update the current best chain tip for
    /// [`LatestChainTip`] and [`ChainTipChange`].
    chain_tip_sender: ChainTipSender,

    /// A sender channel used to update the recent non-finalized state for the [`ReadStateService`].
    non_finalized_state_sender: watch::Sender<NonFinalizedState>,

    /// A cloneable [`ReadStateService`], used to answer concurrent read requests.
    ///
    /// TODO: move users of read [`Request`]s to [`ReadStateService`], and remove `read_service`.
    read_service: ReadStateService,

    // Metrics
    //
    /// A metric tracking the maximum height that's currently in `queued_finalized_blocks`
    ///
    /// Set to `f64::NAN` if `queued_finalized_blocks` is empty, because grafana shows NaNs
    /// as a break in the graph.
    max_queued_height: f64,
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
/// It allows other async tasks to make progress while concurrently reading data from disk.
#[derive(Clone, Debug)]
pub struct ReadStateService {
    // Configuration
    //
    /// The configured Zcash network.
    network: Network,

    // Shared Concurrently Readable State
    //
    /// A watch channel for a recent [`NonFinalizedState`].
    ///
    /// This state is only updated between requests,
    /// so it might include some block data that is also on `disk`.
    non_finalized_state_receiver: WatchReceiver<NonFinalizedState>,

    /// The shared inner on-disk database for the finalized state.
    ///
    /// RocksDB allows reads and writes via a shared reference,
    /// but [`ZebraDb`] doesn't expose any write methods or types.
    ///
    /// This chain is updated concurrently with requests,
    /// so it might include some block data that is also in `best_mem`.
    db: ZebraDb,
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

        let (read_service, non_finalized_state_sender) = ReadStateService::new(&disk);

        let queued_non_finalized_blocks = QueuedBlocks::default();
        let pending_utxos = PendingUtxos::default();

        let state = Self {
            network,
            queued_non_finalized_blocks,
            queued_finalized_blocks: HashMap::new(),
            mem,
            disk,
            pending_utxos,
            last_prune: Instant::now(),
            chain_tip_sender,
            non_finalized_state_sender,
            read_service: read_service.clone(),
            max_queued_height: f64::NAN,
        };
        timer.finish(module_path!(), line!(), "initializing state service");

        tracing::info!("starting legacy chain check");
        let timer = CodeTimer::start();

        if let Some(tip) = state.best_tip() {
            let nu5_activation_height = NetworkUpgrade::Nu5
                .activation_height(network)
                .expect("NU5 activation height is set");

            if let Err(error) = check::legacy_chain(
                nu5_activation_height,
                state.any_ancestor_blocks(tip.1),
                state.network,
            ) {
                let legacy_db_path = state.disk.path().to_path_buf();
                panic!(
                    "Cached state contains a legacy chain.\n\
                     An outdated Zebra version did not know about a recent network upgrade,\n\
                     so it followed a legacy chain using outdated consensus branch rules.\n\
                     Hint: Delete your database, and restart Zebra to do a full sync.\n\
                     Database path: {legacy_db_path:?}\n\
                     Error: {error:?}",
                );
            }
        }

        tracing::info!("cached state consensus branch is valid: no legacy chain found");
        timer.finish(module_path!(), line!(), "legacy chain check");

        (state, read_service, latest_chain_tip, chain_tip_change)
    }

    /// Queue a finalized block for verification and storage in the finalized state.
    fn queue_and_commit_finalized(
        &mut self,
        finalized: FinalizedBlock,
    ) -> oneshot::Receiver<Result<block::Hash, BoxError>> {
        let (rsp_tx, rsp_rx) = oneshot::channel();

        // TODO: move this code into the state block commit task:
        //   - queue_and_commit_finalized()'s commit_finalized() call becomes a send to the block commit channel
        //   - run commit_finalized() in the state block commit task
        //   - run the metrics update in queue_and_commit_finalized() in the block commit task
        //   - run the set_finalized_tip() in this function in the state block commit task
        //   - move all that code to the inner service
        let tip_block = self
            .drain_queue_and_commit_finalized((finalized, rsp_tx))
            .map(ChainTipBlock::from);

        self.chain_tip_sender.set_finalized_tip(tip_block);

        rsp_rx
    }

    /// Queue a finalized block to be committed to the state.
    ///
    /// After queueing a finalized block, this method checks whether the newly
    /// queued block (and any of its descendants) can be committed to the state.
    ///
    /// Returns the highest finalized tip block committed from the queue,
    /// or `None` if no blocks were committed in this call.
    /// (Use `tip_block` to get the finalized tip, regardless of when it was committed.)
    pub fn drain_queue_and_commit_finalized(
        &mut self,
        queued: QueuedFinalized,
    ) -> Option<FinalizedBlock> {
        let mut highest_queue_commit = None;

        let prev_hash = queued.0.block.header.previous_block_hash;
        let height = queued.0.height;
        self.queued_finalized_blocks.insert(prev_hash, queued);

        while let Some(queued_block) = self
            .queued_finalized_blocks
            .remove(&self.disk.db().finalized_tip_hash())
        {
            if let Ok(finalized) = self.disk.commit_finalized(queued_block) {
                highest_queue_commit = Some(finalized);
            } else {
                // the last block in the queue failed, so we can't commit the next block
                break;
            }
        }

        if self.queued_finalized_blocks.is_empty() {
            self.max_queued_height = f64::NAN;
        } else if self.max_queued_height.is_nan() || self.max_queued_height < height.0 as f64 {
            // if there are still blocks in the queue, then either:
            //   - the new block was lower than the old maximum, and there was a gap before it,
            //     so the maximum is still the same (and we skip this code), or
            //   - the new block is higher than the old maximum, and there is at least one gap
            //     between the finalized tip and the new maximum
            self.max_queued_height = height.0 as f64;
        }

        metrics::gauge!("state.checkpoint.queued.max.height", self.max_queued_height);
        metrics::gauge!(
            "state.checkpoint.queued.block.count",
            self.queued_finalized_blocks.len() as f64,
        );

        highest_queue_commit
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
        let rsp_rx = if let Some((_, old_rsp_tx)) =
            self.queued_non_finalized_blocks.get_mut(&prepared.hash)
        {
            tracing::debug!("replacing older queued request with new request");
            let (mut rsp_tx, rsp_rx) = oneshot::channel();
            std::mem::swap(old_rsp_tx, &mut rsp_tx);
            let _ = rsp_tx.send(Err("replaced by newer request".into()));
            rsp_rx
        } else {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            self.queued_non_finalized_blocks.queue((prepared, rsp_tx));
            rsp_rx
        };

        // TODO: avoid a temporary verification failure that can happen
        //       if the first non-finalized block arrives before the last finalized block is committed
        //       (#5125)
        if !self.can_fork_chain_at(&parent_hash) {
            tracing::trace!("unready to verify, returning early");
            return rsp_rx;
        }

        // TODO: move this code into the state block commit task:
        //   - process_queued()'s validate_and_commit() call becomes a send to the block commit channel
        //   - run validate_and_commit() in the state block commit task
        //   - run all the rest of the code in this function in the state block commit task
        //   - move all that code to the inner service
        self.process_queued(parent_hash);

        while self.mem.best_chain_len() > crate::constants::MAX_BLOCK_REORG_HEIGHT {
            tracing::trace!("finalizing block past the reorg limit");
            let finalized_with_trees = self.mem.finalize();
            self.disk
                .commit_finalized_direct(finalized_with_trees, "best non-finalized chain root")
                .expect(
                    "expected that errors would not occur when writing to disk or updating note commitment and history trees",
                );
        }

        let finalized_tip_height = self.disk.db().finalized_tip_height().expect(
            "Finalized state must have at least one block before committing non-finalized state",
        );
        self.queued_non_finalized_blocks
            .prune_by_height(finalized_tip_height);

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

    /// Update the [`LatestChainTip`], [`ChainTipChange`], and `non_finalized_state_sender`
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

        // If the final receiver was just dropped, ignore the error.
        let _ = self.non_finalized_state_sender.send(self.mem.clone());

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
            let queued_children = self
                .queued_non_finalized_blocks
                .dequeue_children(parent_hash);

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
    /// non-finalized [`Chain`](non_finalized_state::Chain).
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

    /// Return the tip of the current best chain.
    pub fn best_tip(&self) -> Option<(block::Height, block::Hash)> {
        self.mem.best_tip().or_else(|| self.disk.db().tip())
    }

    /// Return the height for the block at `hash` in any chain.
    pub fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.mem
            .any_height_by_hash(hash)
            .or_else(|| self.disk.db().height(hash))
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
    /// and a watch channel for updating the shared recent non-finalized chain.
    pub(crate) fn new(disk: &FinalizedState) -> (Self, watch::Sender<NonFinalizedState>) {
        let (non_finalized_state_sender, non_finalized_state_receiver) =
            watch::channel(NonFinalizedState::new(disk.network()));

        let read_service = Self {
            network: disk.network(),
            db: disk.db().clone(),
            non_finalized_state_receiver: WatchReceiver::new(non_finalized_state_receiver),
        };

        tracing::info!("created new read-only state service");

        (read_service, non_finalized_state_sender)
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
            // Uses queued_non_finalized_blocks and pending_utxos in the StateService
            // Accesses shared writeable state in the StateService, NonFinalizedState, and ZebraDb.
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

            // Uses queued_finalized_blocks and pending_utxos in the StateService.
            // Accesses shared writeable state in the StateService and ZebraDb.
            Request::CommitFinalizedBlock(finalized) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "commit_finalized_block",
                );

                let timer = CodeTimer::start();

                // # Consensus
                //
                // A non-finalized block verification could have called AwaitUtxo
                // before this finalized block arrived in the state.
                // So we need to check for pending UTXOs here for non-finalized blocks,
                // even though it is redundant for most finalized blocks.
                // (Finalized blocks are verified using block hash checkpoints
                // and transaction merkle tree block header commitments.)
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

            // Uses pending_utxos and queued_non_finalized_blocks in the StateService.
            // If the UTXO isn't in the queued blocks, runs concurrently using the ReadStateService.
            Request::AwaitUtxo(outpoint) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "await_utxo",
                );

                let timer = CodeTimer::start();

                // Prepare the AwaitUtxo future from PendingUxtos.
                let response_fut = self.pending_utxos.queue(outpoint);
                // Only instrument `response_fut`, the ReadStateService already
                // instruments its requests with the same span.
                let span = Span::current();
                let response_fut = response_fut.instrument(span).boxed();

                // Check the non-finalized block queue outside the returned future,
                // so we can access mutable state fields.
                if let Some(utxo) = self.queued_non_finalized_blocks.utxo(&outpoint) {
                    self.pending_utxos.respond(&outpoint, utxo);

                    // We're finished, the returned future gets the UTXO from the respond() channel.
                    timer.finish(module_path!(), line!(), "AwaitUtxo/queued-non-finalized");

                    return response_fut;
                }

                // We ignore any UTXOs in FinalizedState.queued_finalized_blocks,
                // because it is only used during checkpoint verification.
                //
                // This creates a rare race condition, but it doesn't seem to happen much in practice.
                // See #5126 for details.

                // Manually send a request to the ReadStateService,
                // to get UTXOs from any non-finalized chain or the finalized chain.
                let read_service = self.read_service.clone();

                // Run the request in an async block, so we can await the response.
                async move {
                    let req = ReadRequest::AnyChainUtxo(outpoint);

                    let rsp = read_service.oneshot(req).await?;

                    // Optional TODO:
                    //  - make pending_utxos.respond() async using a channel,
                    //    so we can respond to all waiting requests here
                    //
                    // This change is not required for correctness, because:
                    // - any waiting requests should have returned when the block was sent to the state
                    // - otherwise, the request returns immediately if:
                    //   - the block is in the non-finalized queue, or
                    //   - the block is in any non-finalized chain or the finalized state
                    //
                    // And if the block is in the finalized queue,
                    // that's rare enough that a retry is ok.
                    if let ReadResponse::AnyChainUtxo(Some(utxo)) = rsp {
                        // We got a UTXO, so we replace the response future with the result own.
                        timer.finish(module_path!(), line!(), "AwaitUtxo/any-chain");

                        return Ok(Response::Utxo(utxo));
                    }

                    // We're finished, but the returned future is waiting on the respond() channel.
                    timer.finish(module_path!(), line!(), "AwaitUtxo/waiting");

                    response_fut.await
                }
                .boxed()
            }

            // TODO: add a name() method to Request, and combine all the generic read requests
            //
            // Runs concurrently using the ReadStateService
            Request::Depth(_) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "depth",
                );

                // Redirect the request to the concurrent ReadStateService
                let read_service = self.read_service.clone();

                async move {
                    let req = req
                        .try_into()
                        .expect("ReadRequest conversion should not fail");

                    let rsp = read_service.oneshot(req).await?;
                    let rsp = rsp.try_into().expect("Response conversion should not fail");

                    Ok(rsp)
                }
                .boxed()
            }

            // Runs concurrently using the ReadStateService
            Request::Tip => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "tip",
                );

                // Redirect the request to the concurrent ReadStateService
                let read_service = self.read_service.clone();

                async move {
                    let req = req
                        .try_into()
                        .expect("ReadRequest conversion should not fail");

                    let rsp = read_service.oneshot(req).await?;
                    let rsp = rsp.try_into().expect("Response conversion should not fail");

                    Ok(rsp)
                }
                .boxed()
            }

            // Runs concurrently using the ReadStateService
            Request::BlockLocator => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "block_locator",
                );

                // Redirect the request to the concurrent ReadStateService
                let read_service = self.read_service.clone();

                async move {
                    let req = req
                        .try_into()
                        .expect("ReadRequest conversion should not fail");

                    let rsp = read_service.oneshot(req).await?;
                    let rsp = rsp.try_into().expect("Response conversion should not fail");

                    Ok(rsp)
                }
                .boxed()
            }

            // Runs concurrently using the ReadStateService
            Request::Transaction(_) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "transaction",
                );

                // Redirect the request to the concurrent ReadStateService
                let read_service = self.read_service.clone();

                async move {
                    let req = req
                        .try_into()
                        .expect("ReadRequest conversion should not fail");

                    let rsp = read_service.oneshot(req).await?;
                    let rsp = rsp.try_into().expect("Response conversion should not fail");

                    Ok(rsp)
                }
                .boxed()
            }

            // Runs concurrently using the ReadStateService
            Request::Block(_) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "block",
                );

                // Redirect the request to the concurrent ReadStateService
                let read_service = self.read_service.clone();

                async move {
                    let req = req
                        .try_into()
                        .expect("ReadRequest conversion should not fail");

                    let rsp = read_service.oneshot(req).await?;
                    let rsp = rsp.try_into().expect("Response conversion should not fail");

                    Ok(rsp)
                }
                .boxed()
            }

            // Runs concurrently using the ReadStateService
            Request::FindBlockHashes { .. } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "find_block_hashes",
                );

                // Redirect the request to the concurrent ReadStateService
                let read_service = self.read_service.clone();

                async move {
                    let req = req
                        .try_into()
                        .expect("ReadRequest conversion should not fail");

                    let rsp = read_service.oneshot(req).await?;
                    let rsp = rsp.try_into().expect("Response conversion should not fail");

                    Ok(rsp)
                }
                .boxed()
            }

            // Runs concurrently using the ReadStateService
            Request::FindBlockHeaders { .. } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "state",
                    "type" => "find_block_headers",
                );

                // Redirect the request to the concurrent ReadStateService
                let read_service = self.read_service.clone();

                async move {
                    let req = req
                        .try_into()
                        .expect("ReadRequest conversion should not fail");

                    let rsp = read_service.oneshot(req).await?;
                    let rsp = rsp.try_into().expect("Response conversion should not fail");

                    Ok(rsp)
                }
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
            // Used by the StateService.
            ReadRequest::Tip => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "tip",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let tip = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::tip(non_finalized_state.best_chain(), &state.db)
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::Tip");

                        Ok(ReadResponse::Tip(tip))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Tip"))
                .boxed()
            }

            // Used by the StateService.
            ReadRequest::Depth(hash) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "depth",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let depth = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::depth(non_finalized_state.best_chain(), &state.db, hash)
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::Depth");

                        Ok(ReadResponse::Depth(depth))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Tip"))
                .boxed()
            }

            // Used by get_block RPC and the StateService.
            ReadRequest::Block(hash_or_height) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "block",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let block = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::block(
                                    non_finalized_state.best_chain(),
                                    &state.db,
                                    hash_or_height,
                                )
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::Block");

                        Ok(ReadResponse::Block(block))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Block"))
                .boxed()
            }

            // For the get_raw_transaction RPC and the StateService.
            ReadRequest::Transaction(hash) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "transaction",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let transaction_and_height = state
                            .non_finalized_state_receiver
                            .with_watch_data(|non_finalized_state| {
                                read::transaction(non_finalized_state.best_chain(), &state.db, hash)
                            });

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::Transaction");

                        Ok(ReadResponse::Transaction(transaction_and_height))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Transaction"))
                .boxed()
            }

            // Currently unused.
            ReadRequest::BestChainUtxo(outpoint) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "best_chain_utxo",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let utxo = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::utxo(non_finalized_state.best_chain(), &state.db, outpoint)
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::BestChainUtxo");

                        Ok(ReadResponse::BestChainUtxo(utxo))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::BestChainUtxo"))
                .boxed()
            }

            // Manually used by the StateService to implement part of AwaitUtxo.
            ReadRequest::AnyChainUtxo(outpoint) => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "any_chain_utxo",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let utxo = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::any_utxo(non_finalized_state, &state.db, outpoint)
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::AnyChainUtxo");

                        Ok(ReadResponse::AnyChainUtxo(utxo))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::AnyChainUtxo"))
                .boxed()
            }

            // Used by the StateService.
            ReadRequest::BlockLocator => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "block_locator",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let block_locator = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::block_locator(non_finalized_state.best_chain(), &state.db)
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::BlockLocator");

                        Ok(ReadResponse::BlockLocator(
                            block_locator.unwrap_or_default(),
                        ))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Tip"))
                .boxed()
            }

            // Used by the StateService.
            ReadRequest::FindBlockHashes { known_blocks, stop } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "find_block_hashes",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let block_hashes = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::find_chain_hashes(
                                    non_finalized_state.best_chain(),
                                    &state.db,
                                    known_blocks,
                                    stop,
                                    MAX_FIND_BLOCK_HASHES_RESULTS,
                                )
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::FindBlockHashes");

                        Ok(ReadResponse::BlockHashes(block_hashes))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Tip"))
                .boxed()
            }

            // Used by the StateService.
            ReadRequest::FindBlockHeaders { known_blocks, stop } => {
                metrics::counter!(
                    "state.requests",
                    1,
                    "service" => "read_state",
                    "type" => "find_block_headers",
                );

                let timer = CodeTimer::start();

                let state = self.clone();

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let block_headers = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::find_chain_headers(
                                    non_finalized_state.best_chain(),
                                    &state.db,
                                    known_blocks,
                                    stop,
                                    MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_ZEBRA,
                                )
                            },
                        );

                        let block_headers = block_headers
                            .into_iter()
                            .map(|header| CountedHeader { header })
                            .collect();

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::FindBlockHeaders");

                        Ok(ReadResponse::BlockHeaders(block_headers))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::Tip"))
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

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let sapling_tree = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::sapling_tree(
                                    non_finalized_state.best_chain(),
                                    &state.db,
                                    hash_or_height,
                                )
                            },
                        );

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

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let orchard_tree = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::orchard_tree(
                                    non_finalized_state.best_chain(),
                                    &state.db,
                                    hash_or_height,
                                )
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::OrchardTree");

                        Ok(ReadResponse::OrchardTree(orchard_tree))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::OrchardTree"))
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

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let balance = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::transparent_balance(
                                    non_finalized_state.best_chain().cloned(),
                                    &state.db,
                                    addresses,
                                )
                            },
                        )?;

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::AddressBalance");

                        Ok(ReadResponse::AddressBalance(balance))
                    })
                })
                .map(|join_result| join_result.expect("panic in ReadRequest::AddressBalance"))
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

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let tx_ids = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::transparent_tx_ids(
                                    non_finalized_state.best_chain(),
                                    &state.db,
                                    addresses,
                                    height_range,
                                )
                            },
                        );

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

                let span = Span::current();
                tokio::task::spawn_blocking(move || {
                    span.in_scope(move || {
                        let utxos = state.non_finalized_state_receiver.with_watch_data(
                            |non_finalized_state| {
                                read::address_utxos(
                                    state.network,
                                    non_finalized_state.best_chain(),
                                    &state.db,
                                    addresses,
                                )
                            },
                        );

                        // The work is done in the future.
                        timer.finish(module_path!(), line!(), "ReadRequest::UtxosByAddresses");

                        utxos.map(ReadResponse::AddressUtxos)
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
