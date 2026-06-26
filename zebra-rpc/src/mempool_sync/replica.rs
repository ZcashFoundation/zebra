//! The [`MempoolReplica`]: a follower's local projection of the source mempool's
//! verified and queued sets, built incrementally by applying lifecycle events
//! (design §3a, §6).

use std::collections::{HashMap, HashSet};

use zebra_chain::transaction::{UnminedTx, UnminedTxId};
use zebra_node_services::mempool::{
    replica_digest, QueuedStage, RemovedReason, REPLICA_DIGEST_LEN,
};

/// A verified transaction held in a [`MempoolReplica`], carrying the content and
/// the fee/sigop/ZIP-317 metadata a follower needs without a network round-trip
/// (design §6). Reconstructed from a wire `Added` event.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedReplicaTx {
    /// The unmined transaction, reconstructed from the streamed content.
    pub transaction: UnminedTx,
    /// The miner fee in zatoshi.
    pub miner_fee: u64,
    /// The legacy transparent sigop count.
    pub legacy_sigop_count: u32,
    /// The P2SH redeem-script sigop count.
    pub p2sh_sigop_count: u32,
    /// The number of conventional actions, as defined by ZIP-317.
    pub conventional_actions: u32,
    /// The number of unpaid actions, as defined by ZIP-317 for block production.
    pub unpaid_actions: u32,
    /// The fee weight ratio, as defined by ZIP-317 for block production.
    pub fee_weight_ratio: f32,
}

/// A follower's local replica of the source mempool's derivable state: the
/// verified set (the actual mempool, with metadata) and the queued set
/// (`{txid → stage}`), both keyed by the full [`UnminedTxId`] (design §9.1).
///
/// The replica is built purely by applying lifecycle events incrementally (design
/// §3a). Apply is idempotent and lifecycle-monotonic (design §10): a transaction
/// never regresses to an earlier stage within its current generation, and a
/// removal starts a new generation. The replica recomputes a [`replica_digest`]
/// over its projection ([`Self::digest`]) to compare against each batch's
/// source-computed checksum.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MempoolReplica {
    /// The verified mempool, keyed by full [`UnminedTxId`].
    verified: HashMap<UnminedTxId, VerifiedReplicaTx>,
    /// The queued set (transactions in the download/verify pipeline), keyed by
    /// full [`UnminedTxId`].
    queued: HashMap<UnminedTxId, QueuedStage>,
    /// Content retained for transactions a reorg moved back to the queued set for
    /// re-verification, so the replica can still serve them while they are
    /// re-verified (design §5a "content is retained"). Its keys are always a
    /// subset of [`Self::queued`], so it never affects [`Self::digest`].
    retained: HashMap<UnminedTxId, VerifiedReplicaTx>,
}

/// A single transaction-lifecycle transition observed by a [`MempoolReplica`],
/// republished on the follower's observation feed (design §3b, §6).
#[derive(Clone, Debug)]
pub enum MempoolObservation {
    /// A transaction entered the download/verify pipeline at a stage.
    Queued {
        /// The affected transaction.
        tx_id: UnminedTxId,
        /// The pipeline stage it reached.
        stage: QueuedStage,
    },
    /// A transaction was added to the verified mempool.
    Added {
        /// The affected transaction.
        tx_id: UnminedTxId,
    },
    /// A transaction was removed from the mempool pipeline.
    Removed {
        /// The affected transaction.
        tx_id: UnminedTxId,
        /// Why it was removed.
        reason: RemovedReason,
    },
    /// The stream was (re)started; transient observations may have been missed
    /// during the gap (design §3b). The verified and queued sets are repaired by
    /// re-bootstrapping, but lost transient observations are unrecoverable.
    Gap,
}

impl MempoolReplica {
    /// Returns the lifecycle rank of `id` in its current generation, or `None` if
    /// the replica is not currently tracking it. Ranks increase along the
    /// lifecycle: `Queued{AwaitingDownload}` (0) `< Queued{AwaitingVerification}`
    /// (1) `< verified` (2), matching the source's `MempoolChange::stage_rank`.
    fn current_rank(&self, id: &UnminedTxId) -> Option<u8> {
        if self.verified.contains_key(id) {
            return Some(2);
        }
        self.queued.get(id).map(|stage| match stage {
            QueuedStage::AwaitingDownload => 0,
            QueuedStage::AwaitingVerification => 1,
        })
    }

    /// Applies a `Queued{stage}` event, advancing `id` to `stage` unless that
    /// would regress it within its current generation (design §10).
    ///
    /// Returns the resulting transition, or `None` if the event was a stale
    /// regression or a no-op (idempotent re-apply at the same stage).
    pub fn apply_queued(
        &mut self,
        id: UnminedTxId,
        stage: QueuedStage,
    ) -> Option<MempoolObservation> {
        let incoming = match stage {
            QueuedStage::AwaitingDownload => 0,
            QueuedStage::AwaitingVerification => 1,
        };

        // Never regress within a generation, and treat an identical stage as an
        // idempotent no-op. A verified tx (rank 2) is never dragged back by a
        // stale queued event; only a reorg (`apply_removed` with `Reorged`)
        // re-queues a verified tx.
        if let Some(current) = self.current_rank(&id) {
            if incoming <= current {
                return None;
            }
        }

        self.queued.insert(id, stage);
        Some(MempoolObservation::Queued { tx_id: id, stage })
    }

    /// Applies an `Added` event, moving `id` into the verified set with its
    /// content. `Added` is the highest lifecycle rank, so it always applies
    /// (refreshing content on an idempotent re-apply).
    ///
    /// Returns the transition only when this is a real entry into the verified
    /// set (not an idempotent re-apply of an already-verified transaction).
    pub fn apply_added(
        &mut self,
        id: UnminedTxId,
        tx: VerifiedReplicaTx,
    ) -> Option<MempoolObservation> {
        self.queued.remove(&id);
        self.retained.remove(&id);
        let is_new = self.verified.insert(id, tx).is_none();
        is_new.then_some(MempoolObservation::Added { tx_id: id })
    }

    /// Applies a `Removed{reason}` event.
    ///
    /// A [`RemovedReason::Reorged`] removal is the reorg marker (design §5a): it
    /// re-queues a transaction at `AwaitingDownload` rather than dropping it,
    /// retaining a verified transaction's content. A removal starts a new
    /// generation (design §10), so this re-queue is allowed to regress an
    /// already-queued transaction's stage. `AwaitingDownload` is the source's
    /// actual re-queue entry stage: a reorg re-enters each retried tx through
    /// `download_if_needed_and_verify`, emitting `Queued{AwaitingDownload}` and
    /// only later advancing to `Queued{AwaitingVerification}`. Mirroring that
    /// entry stage keeps the follower's queued projection equal to the source's
    /// throughout recovery, so the source's following `Queued{AwaitingDownload}`
    /// is an idempotent no-op and `Queued{AwaitingVerification}` advances in
    /// lock-step, and the post-cycle digests match without a re-bootstrap.
    ///
    /// Any other reason removes the transaction from the replica.
    ///
    /// Returns the transition, or `None` if the transaction was not being tracked
    /// (an idempotent remove-absent no-op).
    pub fn apply_removed(
        &mut self,
        id: UnminedTxId,
        reason: RemovedReason,
    ) -> Option<MempoolObservation> {
        if matches!(reason, RemovedReason::Reorged) {
            // Re-queue verified → queued{AwaitingDownload}, retaining content so
            // the replica keeps serving the tx while the source re-verifies it.
            if let Some(content) = self.verified.remove(&id) {
                self.queued.insert(id, QueuedStage::AwaitingDownload);
                self.retained.insert(id, content);
                return Some(MempoolObservation::Removed { tx_id: id, reason });
            }
            // Already queued: the new generation permits regressing it back to the
            // re-queue entry stage so it tracks the source's re-verification.
            if let Some(stage) = self.queued.get_mut(&id) {
                *stage = QueuedStage::AwaitingDownload;
                return Some(MempoolObservation::Removed { tx_id: id, reason });
            }
            return None;
        }

        let removed = self.verified.remove(&id).is_some() || self.queued.remove(&id).is_some();
        self.retained.remove(&id);
        removed.then_some(MempoolObservation::Removed { tx_id: id, reason })
    }

    /// Computes the [`replica_digest`] over this replica's verified set, using the
    /// same shared function the source uses so the two agree bit-for-bit (design
    /// §3a-1, §6).
    pub fn digest(&self) -> [u8; REPLICA_DIGEST_LEN] {
        let verified_ids: HashSet<UnminedTxId> = self.verified.keys().copied().collect();
        replica_digest(&verified_ids)
    }

    /// Returns the verified mempool transaction with the given id, if present.
    pub fn verified_tx(&self, id: &UnminedTxId) -> Option<&VerifiedReplicaTx> {
        self.verified.get(id)
    }

    /// Returns the content of `id` if the replica is holding it, whether currently
    /// verified or retained while being re-verified after a reorg (design §5a).
    pub fn transaction(&self, id: &UnminedTxId) -> Option<&VerifiedReplicaTx> {
        self.verified.get(id).or_else(|| self.retained.get(id))
    }

    /// Returns the queued stage of `id`, if it is in the queued set.
    pub fn queued_stage(&self, id: &UnminedTxId) -> Option<QueuedStage> {
        self.queued.get(id).copied()
    }

    /// Returns `true` if `id` is in the verified mempool.
    pub fn contains_verified(&self, id: &UnminedTxId) -> bool {
        self.verified.contains_key(id)
    }

    /// Returns the verified mempool set, keyed by [`UnminedTxId`].
    pub fn verified(&self) -> &HashMap<UnminedTxId, VerifiedReplicaTx> {
        &self.verified
    }

    /// Returns the queued set (`{txid → stage}`).
    pub fn queued(&self) -> &HashMap<UnminedTxId, QueuedStage> {
        &self.queued
    }

    /// Returns the number of verified transactions.
    pub fn verified_len(&self) -> usize {
        self.verified.len()
    }

    /// Returns the number of queued transactions.
    pub fn queued_len(&self) -> usize {
        self.queued.len()
    }

    /// Returns `true` if the replica is tracking no transactions at all.
    pub fn is_empty(&self) -> bool {
        self.verified.is_empty() && self.queued.is_empty()
    }
}
