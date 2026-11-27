//! Writing blocks to the finalized and non-finalized states.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    oneshot, watch,
};

use tracing::Span;
use zebra_chain::{
    block::{self, Block, Height},
    parallel::tree::NoteCommitmentTrees,
    transaction, transparent,
};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    service::{
        check,
        finalized_state::{FinalizedState, ZebraDb},
        non_finalized_state::{NonFinalizedState, ValidatedChain},
        precomputed_trees::PrecomputedTrees,
        queued_blocks::{QueuedCheckpointVerified, QueuedSemanticallyVerified},
        read, ChainTipBlock, ChainTipSender, InvalidateError, ReconsiderError,
    },
    GetBlockTemplateChainInfo, SemanticallyVerifiedBlock, ValidateContextError,
};

// These types are used in doc links
#[allow(unused_imports)]
use crate::service::{
    chain_tip::{ChainTipChange, LatestChainTip},
    non_finalized_state::Chain,
};

/// The maximum size of the parent error map.
///
/// We allow enough space for multiple concurrent chain forks with errors.
const PARENT_ERROR_MAP_LIMIT: usize = MAX_BLOCK_REORG_HEIGHT as usize * 2;

/// Run contextual validation on the prepared block and add it to the
/// non-finalized state if it is contextually valid.
#[tracing::instrument(
    level = "debug",
    skip(finalized_state, non_finalized_state, prepared),
    fields(
        height = ?prepared.height,
        hash = %prepared.hash,
        chains = non_finalized_state.chain_count()
    )
)]
pub(crate) fn validate_and_commit_non_finalized(
    finalized_state: &ZebraDb,
    non_finalized_state: &mut NonFinalizedState,
    prepared: SemanticallyVerifiedBlock,
) -> Result<(), ValidateContextError> {
    let validated = validate_non_finalized(finalized_state, non_finalized_state, prepared)?;
    non_finalized_state.insert_validated(validated);

    Ok(())
}

/// Runs contextual validation on the prepared block, without changing the non-finalized state.
///
/// Returns the chain with the block, for [`NonFinalizedState::insert_validated`].
fn validate_non_finalized(
    finalized_state: &ZebraDb,
    non_finalized_state: &NonFinalizedState,
    prepared: SemanticallyVerifiedBlock,
) -> Result<ValidatedChain, ValidateContextError> {
    check::initial_contextual_validity(finalized_state, non_finalized_state, &prepared)?;
    let parent_hash = prepared.block.header.previous_block_hash;

    if finalized_state.finalized_tip_hash() == parent_hash {
        non_finalized_state.validate_new_chain(prepared, finalized_state)
    } else {
        non_finalized_state.validate_block(prepared, finalized_state)
    }
}

/// The number of prepares the block write task ran after answering a mining candidate, for
/// tests.
#[cfg(test)]
pub(crate) static PREPARES_RUN: AtomicUsize = AtomicUsize::new(0);

/// The number of prepares that kept a validated chain, for tests.
#[cfg(test)]
pub(crate) static PREPARES_STORED: AtomicUsize = AtomicUsize::new(0);

/// The number of commits that added a prepared chain instead of validating the block, for tests.
#[cfg(test)]
pub(crate) static PREPARED_COMMITS: AtomicUsize = AtomicUsize::new(0);

/// The number of prepared chains that were for a commit's block, but not for the chain set it
/// found, for tests.
#[cfg(test)]
pub(crate) static STALE_PREPARED_COMMITS: AtomicUsize = AtomicUsize::new(0);

/// A block whose chain context the block write task validated after answering its mining
/// candidate, for the commit of the same block.
struct PreparedCommit {
    /// Only a commit of this block object can use `validated`: a block hash doesn't identify
    /// the block's authorizing data.
    block: Arc<Block>,

    /// Only a commit with this hash list object can use `validated`, so the chain's indexes
    /// are the ones that commit builds.
    transaction_hashes: Arc<[transaction::Hash]>,

    /// The chain with the block.
    validated: ValidatedChain,

    /// The root and tip hash of each chain that the block was validated with.
    chain_ends: Vec<(block::Hash, block::Hash)>,
}

impl PreparedCommit {
    /// Returns `true` if `queued` has the block and hash list that this was prepared from.
    fn is_for(&self, queued: &SemanticallyVerifiedBlock) -> bool {
        Arc::ptr_eq(&self.block, &queued.block)
            && Arc::ptr_eq(&self.transaction_hashes, &queued.transaction_hashes)
    }

    /// Returns the chain, if the chain set is still the one it was validated with.
    fn into_current_chain(self, non_finalized_state: &NonFinalizedState) -> Option<ValidatedChain> {
        let is_current = self.chain_ends == non_finalized_state.chain_ends();

        // Every message takes the prepared commit, and only messages change the chain set.
        debug_assert!(
            is_current,
            "a prepared commit is only kept until the next message"
        );
        if !is_current {
            tracing::warn!(
                hash = ?self.block.hash(),
                "a prepared commit was kept past a change to the chain set, so the block is \
                 validated again",
            );
            #[cfg(test)]
            STALE_PREPARED_COMMITS.fetch_add(1, Ordering::SeqCst);

            return None;
        }

        Some(self.validated)
    }
}

/// Contextually validates `block` on the current chain set, without changing the state, for a
/// commit of the same block object with the same `transaction_hashes` object.
///
/// `transaction_hashes` are the hashes of the block's transactions, in order. The block's
/// transactions aren't verified yet, so this first checks what the validation relies on.
///
/// Returns `None` if the block can't be validated, or if it is invalid. Its commit validates it
/// again, and returns the error.
fn prepare_commit(
    finalized_state: &ZebraDb,
    non_finalized_state: &NonFinalizedState,
    block: Arc<Block>,
    transaction_hashes: Arc<[transaction::Hash]>,
) -> Option<PreparedCommit> {
    // A mining candidate has a coinbase height, but a block without one must not stop the block
    // write task.
    let height = block.coinbase_height()?;

    // The chain indexes need one unique hash per transaction.
    if transaction_hashes.len() != block.transactions.len() {
        return None;
    }
    let mut unique_hashes = HashSet::with_capacity(transaction_hashes.len());
    if !transaction_hashes
        .iter()
        .all(|hash| unique_hashes.insert(hash))
    {
        return None;
    }

    // A chain asserts that it doesn't have a transaction already. The block verifier rejects a
    // block that repeats one, but only after this runs.
    let parent_hash = block.header.previous_block_hash;
    let repeats_a_parent_chain_transaction = non_finalized_state.chain_iter().find_map(|chain| {
        let parent_height = *chain.height_by_hash.get(&parent_hash)?;

        Some(transaction_hashes.iter().any(|hash| {
            chain
                .tx_loc_by_hash
                .get(hash)
                .is_some_and(|location| location.height <= parent_height)
        }))
    }) == Some(true);
    if repeats_a_parent_chain_transaction {
        return None;
    }

    let prepared = SemanticallyVerifiedBlock {
        block: block.clone(),
        hash: block.hash(),
        height,
        new_outputs: transparent::new_ordered_outputs(&block, &transaction_hashes),
        transaction_hashes: transaction_hashes.clone(),
        received_time: None,
    };
    let chain_ends = non_finalized_state.chain_ends();
    let validated = validate_non_finalized(finalized_state, non_finalized_state, prepared)
        .inspect_err(|error| tracing::debug!(?error, "could not prepare a block's commit"))
        .ok()?;

    Some(PreparedCommit {
        block,
        transaction_hashes,
        validated,
        chain_ends,
    })
}

/// Finalizes the best chain's root blocks while [`NonFinalizedState::should_finalize_root`].
fn finalize_root_blocks(
    finalized_state: &mut FinalizedState,
    non_finalized_state: &mut NonFinalizedState,
    prev_finalized_note_commitment_trees: &mut Option<NoteCommitmentTrees>,
) {
    while non_finalized_state.should_finalize_root() {
        tracing::trace!("finalizing a block past the reorg or index entry limit");
        let contextually_verified_with_trees = non_finalized_state.finalize();
        *prev_finalized_note_commitment_trees = finalized_state
            .commit_finalized_direct(
                contextually_verified_with_trees,
                prev_finalized_note_commitment_trees.take(),
                "commit contextually-verified request",
            )
            .expect(
                "unexpected finalized block commit error: note commitment and history trees were \
                 already checked by the non-finalized state",
            )
            .1
            .into();
    }
}

/// What the block write task did with a non-finalized block, so the state service can update the
/// blocks and outputs it tracks as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NonFinalizedWriteOutcome {
    /// The block was rejected, or evicted from every chain, so it isn't in the state.
    Rejected(block::Hash),

    /// The block was committed, and the state published to readers contains it.
    Committed(block::Hash),
}

/// Sends the hashes of blocks evicted from the non-finalized state on `non_finalized_rejected_sender`,
/// so the state service accepts them again if the network extends their chain.
///
/// Returns `true` if any blocks were evicted.
fn forget_evicted_blocks(
    non_finalized_state: &mut NonFinalizedState,
    non_finalized_rejected_sender: &UnboundedSender<NonFinalizedWriteOutcome>,
) -> bool {
    let evicted = non_finalized_state.take_evicted_block_hashes();
    for &hash in &evicted {
        // If the receiver was dropped, the state service is shutting down.
        let _ = non_finalized_rejected_sender.send(NonFinalizedWriteOutcome::Rejected(hash));
    }

    !evicted.is_empty()
}

/// Finalizes root blocks after a commit, then reports and publishes any forks that finalizing
/// evicted, because [`NonFinalizedState::finalize`] re-inserts the remaining chains under the
/// fork limits.
fn finalize_root_blocks_and_forget_evicted(
    finalized_state: &mut FinalizedState,
    non_finalized_state: &mut NonFinalizedState,
    prev_finalized_note_commitment_trees: &mut Option<NoteCommitmentTrees>,
    non_finalized_rejected_sender: &UnboundedSender<NonFinalizedWriteOutcome>,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    backup_dir_path: Option<&Path>,
) {
    finalize_root_blocks(
        finalized_state,
        non_finalized_state,
        prev_finalized_note_commitment_trees,
    );

    if forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender) {
        update_latest_chain_channels(
            non_finalized_state,
            &finalized_state.db,
            chain_tip_sender,
            non_finalized_state_sender,
            backup_dir_path,
        );
    }
}

/// Update the [`LatestChainTip`], [`ChainTipChange`], and `non_finalized_state_sender`
/// channels with the latest state. If there is no non-finalized chain, use the
/// finalized tip.
///
/// If `backup_dir_path` is `Some`, the non-finalized state is written to the backup
/// directory before updating the channels.
///
/// Returns the latest chain tip height, or `None` if both states are empty.
#[instrument(
    level = "debug",
    skip(
        non_finalized_state,
        finalized_state,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path,
    ),
    fields(chains = non_finalized_state.chain_count())
)]
fn update_latest_chain_channels(
    non_finalized_state: &NonFinalizedState,
    finalized_state: &ZebraDb,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    backup_dir_path: Option<&Path>,
) -> Option<block::Height> {
    // The tip only needs the block's transactions and hashes, so don't copy its UTXO maps.
    let non_finalized_tip = non_finalized_state
        .best_tip_block()
        .map(ChainTipBlock::from);
    let tip_block_height = if let Some(tip) = &non_finalized_tip {
        Some(tip.height)
    } else {
        finalized_state.finalized_tip_height()
    };

    if let Some(backup_dir_path) = backup_dir_path {
        non_finalized_state.write_to_backup(backup_dir_path);
    }

    // If the final receiver was just dropped, ignore the error.
    let _ = non_finalized_state_sender.send(non_finalized_state.clone());

    if let Some(tip) = non_finalized_tip {
        chain_tip_sender.set_best_non_finalized_tip(tip);
    } else {
        let finalized_tip = finalized_state
            .tip_block()
            .expect("the finalized tip must have a complete stored body")
            .map(crate::CheckpointVerifiedBlock::from)
            .map(ChainTipBlock::from);
        chain_tip_sender.clear_best_non_finalized_tip(finalized_tip);
    }

    tip_block_height
}

/// A worker task that reads, validates, and writes blocks to the
/// `finalized_state` or `non_finalized_state`.
struct WriteBlockWorkerTask {
    finalized_block_write_receiver: UnboundedReceiver<QueuedCheckpointVerified>,
    non_finalized_block_write_receiver: UnboundedReceiver<NonFinalizedWriteMessage>,
    finalized_state: FinalizedState,
    non_finalized_state: NonFinalizedState,
    /// Signals the [`crate::service::StateService`] that a finalized block commit failed,
    /// carrying the durable finalized tip height and hash to reset to.
    invalid_block_reset_sender: UnboundedSender<(Option<block::Height>, block::Hash)>,
    /// Signals the [`crate::service::StateService`] that a non-finalized block was rejected by
    /// the write task, so its hash should be removed from
    /// `non_finalized_block_write_sent_hashes`.
    ///
    /// Without this, a rejected same-hash block locks out a later honest
    /// re-delivery of a block at the same hash as a "duplicate" until restart
    /// or reorg.
    non_finalized_rejected_sender: UnboundedSender<NonFinalizedWriteOutcome>,
    chain_tip_sender: ChainTipSender,
    non_finalized_state_sender: watch::Sender<NonFinalizedState>,
    /// If `Some`, the non-finalized state is written to this backup directory
    /// synchronously before each channel update, instead of via the async backup task.
    backup_dir_path: Option<PathBuf>,
}

/// The message type for the non-finalized block write task channel.
pub enum NonFinalizedWriteMessage {
    /// A newly downloaded and semantically verified block prepared for
    /// contextual validation and insertion into the non-finalized state.
    Commit(QueuedSemanticallyVerified),
    /// The hash of a block that should be invalidated and removed from
    /// the non-finalized state, if present.
    Invalidate {
        hash: block::Hash,
        rsp_tx: oneshot::Sender<Result<block::Hash, InvalidateError>>,
    },
    /// The hash of a block that was previously invalidated but should be
    /// reconsidered and reinserted into the non-finalized state.
    Reconsider {
        hash: block::Hash,
        rsp_tx: oneshot::Sender<Result<Vec<block::Hash>, ReconsiderError>>,
    },
    /// A block whose proof of work is valid, but which is still being verified.
    ///
    /// The block write task computes the chain info for mining on top of it, and keeps the
    /// block's note commitment trees and chain history until that block's commit, unless a later
    /// mining candidate's work replaces them. Computing them here, in order with the commits,
    /// means the work starts from the best tip committed when the candidate arrives, and the
    /// commit of the same block never computes its trees at the same time. That commit only
    /// reuses the work if its parent trees and history root are the ones the work started from.
    ///
    /// After answering, the block write task validates the block's chain context with that work,
    /// unless another message is waiting or `verification` is closed. It keeps the result until
    /// its next message, for a [`Self::Commit`] of the same block and hash list objects.
    MiningCandidate {
        block: Arc<block::Block>,

        /// The hashes of the block's transactions, in order.
        transaction_hashes: Arc<[transaction::Hash]>,

        /// Closes once the block verifier is done with the block.
        verification: watch::Receiver<()>,

        rsp_tx: oneshot::Sender<Option<GetBlockTemplateChainInfo>>,
    },
}

impl From<QueuedSemanticallyVerified> for NonFinalizedWriteMessage {
    fn from(block: QueuedSemanticallyVerified) -> Self {
        NonFinalizedWriteMessage::Commit(block)
    }
}

/// A worker with a task that reads, validates, and writes blocks to the
/// `finalized_state` or `non_finalized_state` and channels for sending
/// it blocks.
#[derive(Clone, Debug)]
pub(super) struct BlockWriteSender {
    /// A channel to send blocks to the `block_write_task`,
    /// so they can be written to the [`NonFinalizedState`].
    pub non_finalized: Option<tokio::sync::mpsc::UnboundedSender<NonFinalizedWriteMessage>>,

    /// A channel to send blocks to the `block_write_task`,
    /// so they can be written to the [`FinalizedState`].
    ///
    /// This sender is dropped after the state has finished sending all the checkpointed blocks,
    /// and the lowest semantically verified block arrives.
    pub finalized: Option<tokio::sync::mpsc::UnboundedSender<QueuedCheckpointVerified>>,
}

impl BlockWriteSender {
    /// Creates a new [`BlockWriteSender`] with the given receivers and states.
    #[instrument(
        level = "debug",
        skip_all,
        fields(
            network = %non_finalized_state.network
        )
    )]
    pub fn spawn(
        finalized_state: FinalizedState,
        non_finalized_state: NonFinalizedState,
        chain_tip_sender: ChainTipSender,
        non_finalized_state_sender: watch::Sender<NonFinalizedState>,
        should_use_finalized_block_write_sender: bool,
        backup_dir_path: Option<PathBuf>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<(Option<block::Height>, block::Hash)>,
        tokio::sync::mpsc::UnboundedReceiver<NonFinalizedWriteOutcome>,
        Option<Arc<std::thread::JoinHandle<()>>>,
    ) {
        // Security: The number of blocks in these channels is limited by
        //           the syncer and inbound lookahead limits.
        let (non_finalized_block_write_sender, non_finalized_block_write_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (finalized_block_write_sender, finalized_block_write_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (invalid_block_reset_sender, invalid_block_write_reset_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (non_finalized_rejected_sender, non_finalized_rejected_receiver) =
            tokio::sync::mpsc::unbounded_channel();

        let span = Span::current();
        let task = std::thread::spawn(move || {
            span.in_scope(|| {
                WriteBlockWorkerTask {
                    finalized_block_write_receiver,
                    non_finalized_block_write_receiver,
                    finalized_state,
                    non_finalized_state,
                    invalid_block_reset_sender,
                    non_finalized_rejected_sender,
                    chain_tip_sender,
                    non_finalized_state_sender,
                    backup_dir_path,
                }
                .run()
            })
        });

        (
            Self {
                non_finalized: Some(non_finalized_block_write_sender),
                finalized: should_use_finalized_block_write_sender
                    .then_some(finalized_block_write_sender),
            },
            invalid_block_write_reset_receiver,
            non_finalized_rejected_receiver,
            Some(Arc::new(task)),
        )
    }
}

impl WriteBlockWorkerTask {
    /// Reads blocks from the channels, writes them to the `finalized_state` or `non_finalized_state`,
    /// sends any errors on the `invalid_block_reset_sender`, then updates the `chain_tip_sender` and
    /// `non_finalized_state_sender`.
    #[instrument(
        level = "debug",
        skip(self),
        fields(
            network = %self.non_finalized_state.network
        )
    )]
    pub fn run(mut self) {
        let Self {
            finalized_block_write_receiver,
            non_finalized_block_write_receiver,
            finalized_state,
            non_finalized_state,
            invalid_block_reset_sender,
            non_finalized_rejected_sender,
            chain_tip_sender,
            non_finalized_state_sender,
            backup_dir_path,
        } = &mut self;

        let mut prev_finalized_note_commitment_trees = None;

        // Write all the finalized blocks sent by the state,
        // until the state closes the finalized block channel's sender.
        while let Some(ordered_block) = finalized_block_write_receiver.blocking_recv() {
            // TODO: split these checks into separate functions

            if invalid_block_reset_sender.is_closed() {
                info!("StateService closed the block reset channel. Is Zebra shutting down?");
                return;
            }

            // Discard any children of invalid blocks in the channel
            //
            // `commit_finalized()` requires blocks in height order.
            // So if there has been a block commit error,
            // we need to drop all the descendants of that block,
            // until we receive a block at the required next height.
            let next_valid_height = finalized_state
                .db
                .finalized_tip_height()
                .map(|height| (height + 1).expect("committed heights are valid"))
                .unwrap_or(Height(0));

            if ordered_block.0.height != next_valid_height {
                debug!(
                    ?next_valid_height,
                    invalid_height = ?ordered_block.0.height,
                    invalid_hash = ?ordered_block.0.hash,
                    "got a block that was the wrong height. \
                     Assuming a parent block failed, and dropping this block",
                );

                // We don't want to send a reset here, because it could overwrite a valid sent hash
                std::mem::drop(ordered_block);
                continue;
            }

            // Try committing the block
            match finalized_state
                .commit_finalized(ordered_block, prev_finalized_note_commitment_trees.take())
            {
                Ok((tip_block, note_commitment_trees)) => {
                    prev_finalized_note_commitment_trees = Some(note_commitment_trees);
                    chain_tip_sender.set_finalized_tip(tip_block);
                }
                Err(error) => {
                    let finalized_tip = finalized_state.db.tip();

                    // The last block in the queue failed, so we can't commit the next block.
                    // Instead, we need to reset the state queue,
                    // and discard any children of the invalid block in the channel.
                    info!(
                        ?error,
                        last_valid_height = ?finalized_tip.map(|tip| tip.0),
                        last_valid_hash = ?finalized_tip.map(|tip| tip.1),
                        "committing a block to the finalized state failed, resetting state queue",
                    );

                    let send_result = invalid_block_reset_sender.send((
                        finalized_state.db.finalized_tip_height(),
                        finalized_state.db.finalized_tip_hash(),
                    ));

                    if send_result.is_err() {
                        info!(
                            "StateService closed the block reset channel. Is Zebra shutting down?"
                        );
                        return;
                    }
                }
            }
        }

        // Do this check even if the channel got closed before any finalized blocks were sent.
        // This can happen if we're past the finalized tip.
        if invalid_block_reset_sender.is_closed() {
            info!("StateService closed the block reset channel. Is Zebra shutting down?");
            return;
        }

        // A restored backup can hold more blocks than the limits allow, so finalize them before
        // committing any new blocks.
        let restored_root = non_finalized_state.root_height();
        finalize_root_blocks(
            finalized_state,
            non_finalized_state,
            &mut prev_finalized_note_commitment_trees,
        );
        if non_finalized_state.root_height() != restored_root {
            forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender);
            update_latest_chain_channels(
                non_finalized_state,
                &finalized_state.db,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
            );
        }

        // Save any errors to propagate down to queued child blocks
        let mut parent_error_map: IndexMap<block::Hash, ValidateContextError> = IndexMap::new();

        // The latest mining candidate's work, kept for its own block's commit.
        let mut candidate_trees: Option<PrecomputedTrees> = None;

        // A block validated after its mining candidate was answered. Its validation used the
        // state as it was then, so every message takes it.
        let mut prepared_commit: Option<PreparedCommit> = None;

        while let Some(msg) = non_finalized_block_write_receiver.blocking_recv() {
            let mut prepared = prepared_commit.take();

            let queued_child_and_rsp_tx = match msg {
                NonFinalizedWriteMessage::Commit(queued_child) => Some(queued_child),
                NonFinalizedWriteMessage::MiningCandidate {
                    block,
                    transaction_hashes,
                    verification,
                    rsp_tx,
                } => {
                    let candidate = read::difficulty::mining_candidate_chain_info(
                        non_finalized_state,
                        &finalized_state.db,
                        &block,
                    );

                    let (chain_info, replaced_trees) = match candidate {
                        Ok((chain_info, trees)) => {
                            #[cfg(any(test, feature = "proptest-impl"))]
                            crate::service::precomputed_trees::MINING_CANDIDATES
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                            (Some(chain_info), candidate_trees.replace(trees))
                        }
                        Err(error) => {
                            tracing::debug!(?error, "block can't be used for early mining work");
                            (None, None)
                        }
                    };
                    let has_work = chain_info.is_some();

                    // The first template on this block waits for this answer, and for nothing
                    // below.
                    let _ = rsp_tx.send(chain_info);
                    // Freeing the replaced work and the discarded prepared commit takes time.
                    drop((replaced_trees, prepared));

                    // Skip the prepare if a message is already waiting. If it is the block's own
                    // commit, that commit validates the block with the same work anyway, and any
                    // other message would discard the prepared commit, so it shouldn't wait for
                    // it. A closed guard means the block verifier is done with the block without
                    // committing it.
                    if has_work
                        && non_finalized_block_write_receiver.is_empty()
                        && verification.has_changed().is_ok()
                    {
                        non_finalized_state.precomputed_trees = candidate_trees.take();
                        prepared_commit = prepare_commit(
                            &finalized_state.db,
                            non_finalized_state,
                            block,
                            transaction_hashes,
                        );
                        candidate_trees = non_finalized_state.precomputed_trees.take();

                        #[cfg(test)]
                        {
                            PREPARES_RUN.fetch_add(1, Ordering::SeqCst);
                            if prepared_commit.is_some() {
                                PREPARES_STORED.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                    }

                    continue;
                }
                NonFinalizedWriteMessage::Invalidate { hash, rsp_tx } => {
                    tracing::info!(?hash, "invalidating a block in the non-finalized state");
                    let _ = rsp_tx.send(non_finalized_state.invalidate_block(hash));
                    None
                }
                NonFinalizedWriteMessage::Reconsider { hash, rsp_tx } => {
                    tracing::info!(?hash, "reconsidering a block in the non-finalized state");
                    let _ = rsp_tx
                        .send(non_finalized_state.reconsider_block(hash, &finalized_state.db));
                    None
                }
            };

            let Some((queued_child, rsp_tx, contextual)) = queued_child_and_rsp_tx else {
                forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender);
                update_latest_chain_channels(
                    non_finalized_state,
                    &finalized_state.db,
                    chain_tip_sender,
                    non_finalized_state_sender,
                    backup_dir_path.as_deref(),
                );
                continue;
            };

            let child_hash = queued_child.hash;
            let parent_hash = queued_child.block.header.previous_block_hash;
            let parent_error = parent_error_map.get(&parent_hash);

            // A commit replaces the chain that it extends. The published snapshot can hold the
            // last reference to that chain. Once the best chain is at the reorg limit, the state
            // holds it instead, because finalizing a block after each commit copies the chain.
            // Freeing a chain or a block takes time proportional to its indexes, so what this
            // commit replaces is kept until its caller has the response.
            let replaced_snapshot = non_finalized_state_sender.borrow().clone();
            let replaced_chain = non_finalized_state.best_chain().cloned();
            let mut replaced_proposal = None;
            let mut spent_candidate_trees = None;

            // If the parent block was marked as rejected, also reject all its children.
            //
            // At this point, we know that all the block's descendants
            // are invalid, because we checked all the consensus rules before
            // committing the failing ancestor block to the non-finalized state.
            //
            // The children are served by other peers, which often queue them before
            // the parent arrives, so they get a distinct error that doesn't carry the
            // parent's misbehaviour score.
            let result = if let Some(parent_error) = parent_error {
                Err(parent_error.for_descendant(parent_hash))
            } else if let Some(contextual) = contextual {
                // The block was validated as a proposal, so only apply it to its parent chain.
                non_finalized_state
                    .commit_prevalidated_contextual(*contextual, &finalized_state.db)
                    .map(|replaced| replaced_proposal = replaced)
            } else if let Some(mut validated) = prepared
                .take_if(|prepared| prepared.is_for(&queued_child))
                .and_then(|prepared| prepared.into_current_chain(non_finalized_state))
            {
                tracing::debug!(?child_hash, "committing a prepared block");
                #[cfg(test)]
                PREPARED_COMMITS.fetch_add(1, Ordering::SeqCst);

                validated.set_received_time(queued_child.received_time);
                non_finalized_state.insert_validated(validated);
                spent_candidate_trees =
                    candidate_trees.take_if(|trees| trees.is_for(&queued_child.block));

                Ok(())
            } else {
                tracing::trace!(?child_hash, "validating queued child");
                // A candidate's work is only for its own block object, so only that block's
                // validation uses it. The validation takes it whatever the result, and it is freed
                // after the commit is answered.
                non_finalized_state.precomputed_trees =
                    candidate_trees.take_if(|trees| trees.is_for(&queued_child.block));
                let result = validate_and_commit_non_finalized(
                    &finalized_state.db,
                    non_finalized_state,
                    queued_child,
                );
                spent_candidate_trees = non_finalized_state.precomputed_trees.take();
                result
            };

            // TODO: fix the test timing bugs that require the result to be sent
            //       after `update_latest_chain_channels()`,
            //       and send the result on rsp_tx here

            if let Err(ref error) = result {
                // If the block is invalid, mark any descendant blocks as rejected.
                parent_error_map.insert(child_hash, error.clone());

                // Make sure the error map doesn't get too big.
                if parent_error_map.len() > PARENT_ERROR_MAP_LIMIT {
                    // We only add one hash at a time, so we only need to remove one extra here.
                    parent_error_map.shift_remove_index(0);
                }

                // Signal the StateService to drop this hash from
                // `non_finalized_block_write_sent_hashes`, so a subsequent
                // re-delivery of a block at the same hash is not short-circuited
                // as a "duplicate" against a rejected variant that never reached
                // any chain.
                //
                // If the receiver was dropped (the StateService is shutting
                // down), ignore the error: the lockout cannot matter once the
                // service exits.
                let _ = non_finalized_rejected_sender
                    .send(NonFinalizedWriteOutcome::Rejected(child_hash));

                // Update the caller with the error.
                let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));

                // Skip the things we only need to do for successfully committed blocks
                continue;
            }

            // A successfully committed block supersedes any contextual error
            // recorded for a different block body with the same header hash.
            parent_error_map.shift_remove(&child_hash);

            forget_evicted_blocks(non_finalized_state, non_finalized_rejected_sender);

            // Committing blocks to the finalized state keeps the same chain,
            // so we can update the chain seen by the rest of the application now.
            //
            // TODO: if this causes state request errors due to chain conflicts,
            //       fix the `service::read` bugs,
            //       or do the channel update after the finalized state commit
            let tip_block_height = update_latest_chain_channels(
                non_finalized_state,
                &finalized_state.db,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
            )
            .expect("just successfully inserted a non-finalized block above");

            // Readers can now find the block's outputs in the published state, so the state
            // service doesn't need its own copies.
            let _ =
                non_finalized_rejected_sender.send(NonFinalizedWriteOutcome::Committed(child_hash));

            // Update the caller with the result.
            let _ = rsp_tx.send(result.map(|()| child_hash).map_err(Into::into));
            drop((
                replaced_snapshot,
                replaced_chain,
                replaced_proposal,
                spent_candidate_trees,
                prepared,
            ));

            // A prepared chain that the best chain has passed can't be used any more.
            non_finalized_state.drop_passed_prepared_chain();

            finalize_root_blocks_and_forget_evicted(
                finalized_state,
                non_finalized_state,
                &mut prev_finalized_note_commitment_trees,
                non_finalized_rejected_sender,
                chain_tip_sender,
                non_finalized_state_sender,
                backup_dir_path.as_deref(),
            );

            // Update the metrics if semantic and contextual validation passes
            //
            // TODO: split this out into a function?
            metrics::counter!("state.full_verifier.committed.block.count").increment(1);
            metrics::counter!("zcash.chain.verified.block.total").increment(1);

            metrics::gauge!("state.full_verifier.committed.block.height")
                .set(tip_block_height.0 as f64);

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because this block write task makes sure that blocks
            // are committed in order.
            metrics::gauge!("zcash.chain.verified.block.height").set(tip_block_height.0 as f64);

            tracing::trace!("finished processing queued block");
        }

        // We're finished receiving non-finalized blocks from the state, and
        // done writing to the finalized state, so we can force it to shut down.
        finalized_state.db.shutdown(true);
        std::mem::drop(self.finalized_state);
    }
}

#[cfg(test)]
mod tests;
