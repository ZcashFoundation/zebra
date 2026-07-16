//! The trailing RPC indexer thread (Thread 3 of the write pipeline, enabled
//! only by the consensus / RPC write split).
//!
//! When [`Config::separate_rpc_index_db`](crate::Config::separate_rpc_index_db)
//! is set, the consensus database (the main [`ZebraDb`]) holds only the
//! consensus-critical column families, committed block-by-block by the disk
//! writer (Thread 2). This thread **trails** the consensus database: it reads
//! each newly durable block from the consensus database and writes that block's
//! RPC-only transparent indexes (address / balance / spent-tx) into the separate
//! RPC index database.
//!
//! The consensus thread never blocks on this thread. The coupling is one
//! [`AtomicU32`] — the disk writer's durable-tip height, the same atomic the
//! worker's prune already reads — which this thread `Acquire`-loads to learn how
//! far it may index.
//!
//! # Crash safety and catch-up
//!
//! The RPC index database carries its own durable tip marker
//! ([`ZebraDb::rpc_index_tip`]). On start this thread catches up from
//! `rpc_index_tip + 1` to the consensus tip, then trails the live durable tip.
//! Each block's index plus the advanced tip marker are written in one atomic RPC
//! -index-DB batch, so the RPC index is never ahead of the consensus database and
//! a crash leaves it at a whole-block boundary. See
//! `docs/design/state-write-split.md` §5.

use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use zebra_chain::{block::Height, parameters::Network};

use crate::service::{finalized_state::ZebraDb, write::NO_DISK_TIP_HEIGHT};

/// How long the indexer parks when it has caught up to the durable tip before
/// re-checking the atomic. Block commit is far slower than this in steady state,
/// so the poll is effectively idle; during IBD the indexer is never caught up.
const RPC_INDEXER_IDLE_POLL: Duration = Duration::from_millis(50);

/// The trailing RPC indexer: reads newly durable blocks from `consensus_db` and
/// writes their RPC-only transparent indexes into `rpc_index_db`.
pub(super) struct RpcIndexer {
    /// The consensus database, the source of durable blocks and spend
    /// resolution. Its `rpc_index_db()` is `Some(rpc_index_db)`.
    pub(super) consensus_db: ZebraDb,

    /// The separate RPC index database, the destination of RPC-only writes. Its
    /// own `rpc_index_db()` is `None`.
    pub(super) rpc_index_db: ZebraDb,

    /// The network, for transparent address derivation.
    pub(super) network: Network,

    /// The disk writer's durable-tip height: the highest height the consensus
    /// database has made durable. This thread never indexes above it.
    ///
    /// # Correctness
    ///
    /// `Acquire`-loaded here, paired with the disk writer's `Release` store
    /// **after** `commit_finalized_direct` returns, so reading height `H` here
    /// happens-after the completion of `H`'s consensus write — the block is
    /// always readable from `consensus_db` before this thread indexes it.
    pub(super) disk_tip_height: Arc<AtomicU32>,

    /// Set by the worker when the state service is shutting down, so this thread
    /// exits its poll loop promptly instead of parking forever.
    pub(super) shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl RpcIndexer {
    /// Runs the trailing indexer until shutdown: catch up to the consensus tip,
    /// then trail the live durable tip.
    pub(super) fn run(self) {
        // Catch up from the RPC index's own durable tip to the consensus tip.
        // The RPC index is never ahead, so this only ever indexes forward.
        let mut next_height = self
            .rpc_index_db
            .rpc_index_tip_height()
            .map(|height| (height + 1).expect("indexed heights are valid"))
            .unwrap_or(Height(0));

        loop {
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }

            // The highest height we may index: the durable consensus tip. The
            // sentinel means nothing is durable yet (empty consensus database).
            let durable = self.disk_tip_height.load(Ordering::Acquire);
            let durable_height = if durable == NO_DISK_TIP_HEIGHT {
                None
            } else {
                Some(Height(durable))
            };

            let Some(durable_height) = durable_height else {
                // Nothing durable yet; wait for the consensus database to commit.
                std::thread::sleep(RPC_INDEXER_IDLE_POLL);
                continue;
            };

            if next_height > durable_height {
                // Caught up; park briefly and re-check.
                std::thread::sleep(RPC_INDEXER_IDLE_POLL);
                continue;
            }

            // Index every durable height we are behind, in order.
            while next_height <= durable_height {
                if self.shutdown.load(Ordering::Acquire) {
                    return;
                }

                if !self.index_height(next_height) {
                    // The block isn't readable yet (a transient race with the
                    // consensus write that the Acquire pairing should prevent,
                    // or a shutdown mid-write). Back off and retry; never skip a
                    // height, so the RPC index stays contiguous.
                    std::thread::sleep(RPC_INDEXER_IDLE_POLL);
                    break;
                }

                next_height = match next_height + 1 {
                    Some(height) => height,
                    None => return,
                };
            }
        }
    }

    /// Indexes the block at `height` from the consensus database into the RPC
    /// index database. Returns `false` if the block can't be read yet (so the
    /// caller retries without advancing).
    fn index_height(&self, height: Height) -> bool {
        let Some(block) = self.consensus_db.block(height.into()) else {
            return false;
        };
        let Some(hash) = self.consensus_db.hash(height) else {
            return false;
        };

        let finalized = crate::request::FinalizedBlock::for_rpc_index(block, hash);

        match self
            .rpc_index_db
            .write_rpc_index_block(&self.consensus_db, &finalized, &self.network)
        {
            Ok(()) => {
                metrics::counter!("state.rpc_index.indexed.block.count").increment(1);
                metrics::gauge!("state.rpc_index.indexed.block.height").set(height.0 as f64);
                true
            }
            Err(error) => {
                // An RPC index write failure is non-fatal: the RPC index is
                // non-consensus data. Log and retry the same height.
                tracing::warn!(
                    ?error,
                    ?height,
                    "failed to write RPC index for block; will retry",
                );
                false
            }
        }
    }
}
