//! Writing blocks to the finalized and non-finalized states.
//!
//! The write pipeline has two stages:
//!
//! - the **worker** (Thread 1, [`worker::WriteBlockWorker`]) commits each block
//!   to the in-memory non-finalized state, and
//! - the **disk writer** (Thread 2, [`disk_writer::DiskWriter`]) commits each
//!   durable-bound block to the finalized state (RocksDB).
//!
//! The worker reads one channel of [`WriteMessage`]s carrying checkpoint and
//! semantic blocks (and invalidate/reconsider admin requests). It acknowledges
//! a checkpoint block as soon as it is in memory, so block commits are not
//! serialized behind disk I/O; a bounded channel limits how many blocks have
//! disk writes in flight.

mod disk_writer;
mod finalized_write_phase;
mod worker;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    oneshot, watch,
};

use tracing::Span;
use zebra_chain::block;

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    service::{
        check,
        finalized_state::{FinalizedState, ZebraDb},
        non_finalized_state::NonFinalizedState,
        queued_blocks::{QueuedCheckpointVerified, QueuedSemanticallyVerified},
        ChainTipBlock, ChainTipSender, InvalidateError, ReconsiderError,
    },
    SemanticallyVerifiedBlock, ValidateContextError,
};

use self::worker::WriteBlockWorker;

// These types are used in doc links
#[allow(unused_imports)]
use crate::service::{
    chain_tip::{ChainTipChange, LatestChainTip},
    non_finalized_state::Chain,
};

/// The maximum size of the parent error map.
///
/// We allow enough space for multiple concurrent chain forks with errors.
pub(super) const PARENT_ERROR_MAP_LIMIT: usize = MAX_BLOCK_REORG_HEIGHT as usize * 2;

/// The minimum value for the checkpoint-sync retention and pipeline
/// configs: half the rollback window.
///
/// See
/// [`Config::checkpoint_sync_retained_blocks`](crate::Config::checkpoint_sync_retained_blocks)
/// and
/// [`Config::checkpoint_sync_pipeline_capacity`](crate::Config::checkpoint_sync_pipeline_capacity);
/// configured values below this are raised to it.
pub(super) const MIN_CHECKPOINT_SYNC_RETAINED_BLOCKS: u32 = MAX_BLOCK_REORG_HEIGHT / 2;

/// The disk-writer tip height value meaning nothing has been written to disk
/// yet: `u32::MAX`, far above any real block height.
pub(super) const NO_DISK_TIP_HEIGHT: u32 = u32::MAX;

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
    check::initial_contextual_validity(finalized_state, non_finalized_state, &prepared)?;
    let parent_hash = prepared.block.header.previous_block_hash;

    if finalized_state.finalized_tip_hash() == parent_hash {
        non_finalized_state.commit_new_chain(prepared, finalized_state)?;
    } else {
        non_finalized_state.commit_block(prepared, finalized_state)?;
    }

    Ok(())
}

/// Update the [`LatestChainTip`], [`ChainTipChange`], and `non_finalized_state_sender`
/// channels with the latest non-finalized [`ChainTipBlock`] and
/// [`Chain`].
///
/// `last_zebra_mined_log_height` is used to rate-limit logging.
///
/// If `backup_dir_path` is `Some`, the non-finalized state is written to the backup
/// directory before updating the channels.
///
/// Returns the latest non-finalized chain tip height.
///
/// # Panics
///
/// If the `non_finalized_state` is empty.
#[instrument(
    level = "debug",
    skip(
        non_finalized_state,
        chain_tip_sender,
        non_finalized_state_sender,
        backup_dir_path,
    ),
    fields(chains = non_finalized_state.chain_count())
)]
pub(super) fn update_latest_chain_channels(
    non_finalized_state: &NonFinalizedState,
    chain_tip_sender: &mut ChainTipSender,
    non_finalized_state_sender: &watch::Sender<NonFinalizedState>,
    backup_dir_path: Option<&Path>,
) -> block::Height {
    let best_chain = non_finalized_state.best_chain().expect("unexpected empty non-finalized state: must commit at least one block before updating channels");

    let tip_block = best_chain
        .tip_block()
        .expect("unexpected empty chain: must commit at least one block before updating channels")
        .clone();
    let tip_block = ChainTipBlock::from(tip_block);

    let tip_block_height = tip_block.height;

    if let Some(backup_dir_path) = backup_dir_path {
        non_finalized_state.write_to_backup(backup_dir_path);
    }

    // If the final receiver was just dropped, ignore the error.
    let _ = non_finalized_state_sender.send(non_finalized_state.clone());

    chain_tip_sender.set_best_non_finalized_tip(tip_block);

    tip_block_height
}

/// A message to the [block write worker](worker::WriteBlockWorker).
///
/// Checkpoint and semantic blocks share one channel so the worker can commit
/// them in any order; the single-threaded `StateService` serializes both
/// streams into commit order before sending, so a FIFO read preserves
/// parent-before-child ordering.
pub(super) enum WriteMessage {
    /// A checkpoint-verified block, committed via the fast in-memory path and
    /// handed to the disk writer.
    Checkpoint(QueuedCheckpointVerified),

    /// A semantically verified block, prepared for full contextual validation
    /// and insertion into the non-finalized state.
    Semantic(QueuedSemanticallyVerified),

    /// The hash of a block to invalidate and remove from the non-finalized
    /// state, if present.
    Invalidate {
        hash: block::Hash,
        rsp_tx: oneshot::Sender<Result<block::Hash, InvalidateError>>,
    },

    /// The hash of a previously invalidated block to reconsider and reinsert
    /// into the non-finalized state.
    Reconsider {
        hash: block::Hash,
        rsp_tx: oneshot::Sender<Result<Vec<block::Hash>, ReconsiderError>>,
    },
}

impl From<QueuedSemanticallyVerified> for WriteMessage {
    fn from(block: QueuedSemanticallyVerified) -> Self {
        WriteMessage::Semantic(block)
    }
}

/// The sending half of the channel to the [block write worker](worker::WriteBlockWorker).
///
/// The sender is wrapped in an `Option` only so [`StateService::drop`] can
/// close it, making the worker exit the next time it reads the channel.
///
/// [`StateService::drop`]: crate::service::StateService
#[derive(Clone, Debug)]
pub(super) struct BlockWriteSender {
    /// The single channel to the block write worker.
    ///
    /// Security: The number of blocks in this channel is limited by the syncer
    /// and inbound lookahead limits, and the IBD engine's commit caps.
    pub sender: Option<UnboundedSender<WriteMessage>>,
}

impl BlockWriteSender {
    /// Spawns the block write worker over `finalized_state` and
    /// `non_finalized_state`, and returns the sender, the invalid-block reset
    /// receiver, the rejected-hash receiver, and the worker's join handle.
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
        backup_dir_path: Option<PathBuf>,
    ) -> (
        Self,
        UnboundedReceiver<block::Hash>,
        UnboundedReceiver<block::Hash>,
        Option<Arc<std::thread::JoinHandle<()>>>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (invalid_block_reset_sender, invalid_block_write_reset_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let (non_finalized_rejected_sender, non_finalized_rejected_receiver) =
            tokio::sync::mpsc::unbounded_channel();

        let span = Span::current();
        let task = std::thread::spawn(move || {
            span.in_scope(|| {
                WriteBlockWorker {
                    receiver,
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
                sender: Some(sender),
            },
            invalid_block_write_reset_receiver,
            non_finalized_rejected_receiver,
            Some(Arc::new(task)),
        )
    }
}
