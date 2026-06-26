//! Defines the [`MempoolChange`] and [`MempoolChangeKind`] types used by the mempool change broadcast channel.

use std::collections::HashSet;

use tokio::sync::broadcast;
use zebra_chain::{block, transaction::UnminedTxId};

/// A newtype around [`broadcast::Sender<MempoolChange>`] used to
/// subscribe to the channel without an active receiver.
#[derive(Clone, Debug)]
pub struct MempoolTxSubscriber(broadcast::Sender<MempoolChange>);

impl MempoolTxSubscriber {
    /// Creates a new [`MempoolTxSubscriber`].
    pub fn new(sender: broadcast::Sender<MempoolChange>) -> Self {
        Self(sender)
    }

    /// Subscribes to the channel, returning a [`broadcast::Receiver`].
    pub fn subscribe(&self) -> broadcast::Receiver<MempoolChange> {
        self.0.subscribe()
    }
}

/// The stage a transaction has reached in the download/verify pipeline before
/// it enters (or fails to enter) the verified mempool.
///
/// The stages are lifecycle-monotonic: a transaction advances
/// `AwaitingDownload` → `AwaitingVerification` → (verified) and never regresses
/// within a single generation (see the lifecycle model in the design doc, §4).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum QueuedStage {
    /// The transaction is queued and awaiting download of its content.
    AwaitingDownload,
    /// The transaction has been downloaded and is awaiting verification.
    AwaitingVerification,
}

/// The reason a transaction was removed from the mempool pipeline.
///
/// This carries all the richness of the lifecycle model. Note that
/// [`RemovedReason::FailedVerification`] carries a **stable string code**, not
/// the structured `zebra_consensus` `TransactionError`: this crate depends only
/// on `zebra-chain`, and a stable code avoids coupling the broadcast type to
/// consensus error shapes (design §9.2).
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RemovedReason {
    /// The transaction's content could not be downloaded.
    FailedDownload,
    /// The transaction did not pass verification. Carries a stable error code.
    FailedVerification(String),
    /// The transaction was mined onto the best chain.
    Mined {
        /// The hash of the block the transaction was mined into.
        block_hash: block::Hash,
        /// The height of the block the transaction was mined into.
        height: block::Height,
    },
    /// The transaction expired before it could be mined.
    Expired,
    /// The transaction was evicted, e.g. by a ZIP-401 cost-limit eviction or a
    /// conflicting transaction that was mined.
    Evicted,
    /// The transaction was re-queued for re-verification after a chain tip reset.
    Reorged,
}

/// Represents a kind of change in a transaction's mempool lifecycle.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MempoolChangeKind {
    /// Transactions entered the download/verify pipeline at the given stage.
    Queued(QueuedStage),
    /// Transactions were added to the verified mempool.
    Added,
    /// Transactions were removed from the mempool pipeline for the given reason.
    Removed(RemovedReason),
}

/// Represents a change in the mempool's lifecycle for a group of transactions.
///
/// A single change carries one lifecycle [`MempoolChangeKind`] shared by all the
/// transactions in `tx_ids`, keyed by their full [`UnminedTxId`] (design §9.1).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MempoolChange {
    /// The kind of change that occurred in the mempool.
    pub kind: MempoolChangeKind,
    /// The set of [`UnminedTxId`]s of transactions that were affected by the change.
    pub tx_ids: HashSet<UnminedTxId>,
}

impl MempoolChange {
    /// Creates a new [`MempoolChange`] with the specified kind and transaction IDs.
    pub fn new(kind: MempoolChangeKind, tx_ids: HashSet<UnminedTxId>) -> Self {
        Self { kind, tx_ids }
    }

    /// Returns a reference to the kind of change that occurred in the mempool.
    pub fn kind(&self) -> &MempoolChangeKind {
        &self.kind
    }

    /// Returns true if the kind of change that occurred in the mempool is [`MempoolChangeKind::Added`].
    pub fn is_added(&self) -> bool {
        matches!(self.kind, MempoolChangeKind::Added)
    }

    /// Consumes self and returns the set of [`UnminedTxId`]s of transactions that were affected by the change.
    pub fn into_tx_ids(self) -> HashSet<UnminedTxId> {
        self.tx_ids
    }

    /// Returns a reference to the set of [`UnminedTxId`]s of transactions that were affected by the change.
    pub fn tx_ids(&self) -> &HashSet<UnminedTxId> {
        &self.tx_ids
    }

    /// Returns the lifecycle stage rank used for monotonic apply (design §10),
    /// or `None` for [`MempoolChangeKind::Removed`] changes, which begin a new
    /// generation. Ranks are strictly increasing along the lifecycle:
    /// `Queued{AwaitingDownload} < Queued{AwaitingVerification} < Added`.
    pub fn stage_rank(&self) -> Option<u8> {
        match &self.kind {
            MempoolChangeKind::Queued(QueuedStage::AwaitingDownload) => Some(0),
            MempoolChangeKind::Queued(QueuedStage::AwaitingVerification) => Some(1),
            MempoolChangeKind::Added => Some(2),
            MempoolChangeKind::Removed(_) => None,
        }
    }

    /// Creates a change indicating that transactions were queued and are awaiting download.
    pub fn queued_awaiting_download(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(
            MempoolChangeKind::Queued(QueuedStage::AwaitingDownload),
            tx_ids,
        )
    }

    /// Creates a change indicating that transactions were downloaded and are awaiting verification.
    pub fn queued_awaiting_verification(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(
            MempoolChangeKind::Queued(QueuedStage::AwaitingVerification),
            tx_ids,
        )
    }

    /// Creates a change indicating that transactions were added to the verified mempool.
    pub fn added(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(MempoolChangeKind::Added, tx_ids)
    }

    /// Creates a change indicating that transactions expired before being mined.
    pub fn removed_expired(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(MempoolChangeKind::Removed(RemovedReason::Expired), tx_ids)
    }

    /// Creates a change indicating that transactions were evicted from the mempool.
    pub fn removed_evicted(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(MempoolChangeKind::Removed(RemovedReason::Evicted), tx_ids)
    }

    /// Creates a change indicating that transactions failed to download.
    pub fn removed_failed_download(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(
            MempoolChangeKind::Removed(RemovedReason::FailedDownload),
            tx_ids,
        )
    }

    /// Creates a change indicating that transactions failed verification, carrying a stable error code.
    pub fn removed_failed_verification(
        tx_ids: HashSet<UnminedTxId>,
        code: impl Into<String>,
    ) -> Self {
        Self::new(
            MempoolChangeKind::Removed(RemovedReason::FailedVerification(code.into())),
            tx_ids,
        )
    }

    /// Creates a change indicating that transactions were mined onto the best chain.
    pub fn removed_mined(
        tx_ids: HashSet<UnminedTxId>,
        block_hash: block::Hash,
        height: block::Height,
    ) -> Self {
        Self::new(
            MempoolChangeKind::Removed(RemovedReason::Mined { block_hash, height }),
            tx_ids,
        )
    }

    /// Creates a change indicating that transactions were re-queued for re-verification after a reorg.
    pub fn removed_reorged(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(MempoolChangeKind::Removed(RemovedReason::Reorged), tx_ids)
    }
}

#[cfg(test)]
mod tests {
    use zebra_chain::transaction;

    use super::*;

    fn txid(byte: u8) -> UnminedTxId {
        UnminedTxId::Legacy(transaction::Hash::from([byte; 32]))
    }

    fn ids(bytes: &[u8]) -> HashSet<UnminedTxId> {
        bytes.iter().map(|&b| txid(b)).collect()
    }

    #[test]
    fn constructors_round_trip() {
        let set = ids(&[1, 2]);

        assert_eq!(
            MempoolChange::queued_awaiting_download(set.clone()).kind(),
            &MempoolChangeKind::Queued(QueuedStage::AwaitingDownload)
        );
        assert_eq!(
            MempoolChange::queued_awaiting_verification(set.clone()).kind(),
            &MempoolChangeKind::Queued(QueuedStage::AwaitingVerification)
        );
        assert_eq!(
            MempoolChange::added(set.clone()).kind(),
            &MempoolChangeKind::Added
        );
        assert_eq!(
            MempoolChange::removed_expired(set.clone()).kind(),
            &MempoolChangeKind::Removed(RemovedReason::Expired)
        );
        assert_eq!(
            MempoolChange::removed_evicted(set.clone()).kind(),
            &MempoolChangeKind::Removed(RemovedReason::Evicted)
        );
        assert_eq!(
            MempoolChange::removed_failed_download(set.clone()).kind(),
            &MempoolChangeKind::Removed(RemovedReason::FailedDownload)
        );
        assert_eq!(
            MempoolChange::removed_reorged(set.clone()).kind(),
            &MempoolChangeKind::Removed(RemovedReason::Reorged)
        );

        let change =
            MempoolChange::removed_mined(set.clone(), block::Hash([7; 32]), block::Height(9));
        assert_eq!(
            change.kind(),
            &MempoolChangeKind::Removed(RemovedReason::Mined {
                block_hash: block::Hash([7; 32]),
                height: block::Height(9),
            })
        );

        // The affected tx ids are preserved on the change.
        assert_eq!(change.tx_ids(), &set);
        assert_eq!(change.into_tx_ids(), set);
    }

    #[test]
    fn failed_verification_carries_stable_code() {
        let change = MempoolChange::removed_failed_verification(ids(&[3]), "bad_balance");

        match change.kind() {
            MempoolChangeKind::Removed(RemovedReason::FailedVerification(code)) => {
                assert_eq!(code, "bad_balance");
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn only_added_is_added() {
        assert!(MempoolChange::added(ids(&[1])).is_added());
        assert!(!MempoolChange::queued_awaiting_download(ids(&[1])).is_added());
        assert!(!MempoolChange::queued_awaiting_verification(ids(&[1])).is_added());
        assert!(!MempoolChange::removed_expired(ids(&[1])).is_added());
    }

    #[test]
    fn stage_rank_is_lifecycle_monotonic() {
        let download = MempoolChange::queued_awaiting_download(ids(&[1]))
            .stage_rank()
            .expect("queued changes have a stage rank");
        let verification = MempoolChange::queued_awaiting_verification(ids(&[1]))
            .stage_rank()
            .expect("queued changes have a stage rank");
        let added = MempoolChange::added(ids(&[1]))
            .stage_rank()
            .expect("added changes have a stage rank");

        // Queued{AwaitingDownload} < Queued{AwaitingVerification} < InMempool (Added).
        assert!(download < verification);
        assert!(verification < added);

        // Removed changes begin a new generation and have no stage rank.
        assert_eq!(MempoolChange::removed_expired(ids(&[1])).stage_rank(), None);
        assert_eq!(
            MempoolChange::removed_failed_verification(ids(&[1]), "x").stage_rank(),
            None
        );
    }
}
