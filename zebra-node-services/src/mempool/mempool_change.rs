//! Defines the [`MempoolChange`] and [`MempoolChangeKind`] types used by the mempool change broadcast channel.

use std::collections::HashSet;

use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use zebra_chain::{block, transaction::UnminedTxId};

/// A newtype around [`broadcast::Sender<MempoolBatch>`] used to
/// subscribe to the channel without an active receiver.
#[derive(Clone, Debug)]
pub struct MempoolTxSubscriber(broadcast::Sender<MempoolBatch>);

impl MempoolTxSubscriber {
    /// Creates a new [`MempoolTxSubscriber`].
    pub fn new(sender: broadcast::Sender<MempoolBatch>) -> Self {
        Self(sender)
    }

    /// Subscribes to the channel, returning a [`broadcast::Receiver`].
    pub fn subscribe(&self) -> broadcast::Receiver<MempoolBatch> {
        self.0.subscribe()
    }
}

/// The length in bytes of a [`replica_digest`] output.
pub const REPLICA_DIGEST_LEN: usize = 32;

/// A per-cycle batch of mempool lifecycle [`MempoolChange`]s, optionally carrying
/// an anti-entropy [`replica_digest`] over the source's verified set (design §5,
/// §9.8).
///
/// The wire unit of mempool synchronization is a batch, not an individual event:
/// the mempool source coalesces every change observed in a single
/// `Mempool::poll_ready` cycle into one [`MempoolBatch`], then stamps it with the
/// post-cycle `checksum` once the cycle's changes have settled. A follower
/// applies the batch's `events` atomically, then recomputes and compares the
/// `checksum`; a mismatch is the divergence detector (design §3a).
///
/// Mid-cycle observations emitted outside the settled `poll_ready` boundary (the
/// download/verify pipeline's `Queued`/`Removed{FailedDownload}` events) carry no
/// `checksum`, because the replica projection is not at a settled point when they
/// are produced.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MempoolBatch {
    /// Every lifecycle event observed in this change cycle, in order.
    pub events: Vec<MempoolChange>,
    /// A [`replica_digest`] over the source's verified set after this batch
    /// settles, or `None` for mid-cycle batches (design §3a-1, §9.8).
    pub checksum: Option<[u8; REPLICA_DIGEST_LEN]>,
}

impl MempoolBatch {
    /// Creates a new [`MempoolBatch`] from the given events and optional checksum.
    pub fn new(events: Vec<MempoolChange>, checksum: Option<[u8; REPLICA_DIGEST_LEN]>) -> Self {
        Self { events, checksum }
    }

    /// Creates a single-event mid-cycle batch with no settled checksum.
    pub fn event(change: MempoolChange) -> Self {
        Self {
            events: vec![change],
            checksum: None,
        }
    }

    /// Returns the lifecycle events carried by this batch.
    pub fn events(&self) -> &[MempoolChange] {
        &self.events
    }

    /// Consumes self and returns the lifecycle events carried by this batch.
    pub fn into_events(self) -> Vec<MempoolChange> {
        self.events
    }

    /// Returns the post-cycle checksum, if this batch carries one.
    pub fn checksum(&self) -> Option<[u8; REPLICA_DIGEST_LEN]> {
        self.checksum
    }
}

/// Encodes one verified-set entry as a fixed-width, order-independent record for
/// [`replica_digest`].
///
/// The record is `[variant_tag, mined_id (32), auth_digest (32)]`, where
/// `variant_tag` distinguishes [`UnminedTxId::Legacy`] (no auth digest) from
/// [`UnminedTxId::Witnessed`], so transactions with the same mined id but
/// different keys never collide.
fn digest_record(id: &UnminedTxId) -> [u8; 65] {
    let mut record = [0u8; 65];
    if let Some(auth) = id.auth_digest() {
        record[0] = 1;
        record[33..65].copy_from_slice(&auth.0);
    }
    record[1..33].copy_from_slice(&id.mined_id().0);
    record
}

/// Computes an order-independent, deterministic digest over the source's verified
/// set (`verified_ids`), per design §3a-1 and §9.6.
///
/// Both the mempool source and a follower compute this digest over their own
/// verified set and compare; a mismatch signals divergence (design §3a). It does
/// **not** cover the queued set, source-internal machinery (download timers, retry
/// counts), nor the transient observation layer (removal reasons): none of those
/// is checksummed retained state.
///
/// The digest is a SHA-256 over the sorted per-entry records (§9.6's sorted-ids
/// hash), so it is independent of iteration order and cheap to recompute on a
/// small set.
pub fn replica_digest(verified_ids: &HashSet<UnminedTxId>) -> [u8; REPLICA_DIGEST_LEN] {
    let mut records: Vec<[u8; 65]> = verified_ids.iter().map(digest_record).collect();
    records.sort_unstable();

    let mut hasher = Sha256::new();
    for record in &records {
        hasher.update(record);
    }
    hasher.finalize().into()
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

    fn wtxid(byte: u8) -> UnminedTxId {
        UnminedTxId::Witnessed(zebra_chain::transaction::WtxId {
            id: transaction::Hash::from([byte; 32]),
            auth_digest: transaction::AuthDigest([byte ^ 0xff; 32]),
        })
    }

    #[test]
    fn replica_digest_is_order_independent() {
        // The same verified set inserted in a different order hashes equally,
        // because the records are sorted before hashing (§9.6).
        let verified: HashSet<_> = [txid(1), wtxid(2), txid(3)].into_iter().collect();
        let verified_reordered: HashSet<_> = [txid(3), txid(1), wtxid(2)].into_iter().collect();

        assert_eq!(
            replica_digest(&verified),
            replica_digest(&verified_reordered)
        );
    }

    #[test]
    fn replica_digest_distinguishes_verified_sets() {
        // Different verified membership yields different digests.
        assert_ne!(
            replica_digest(&[txid(1)].into_iter().collect()),
            replica_digest(&[txid(2)].into_iter().collect())
        );

        // A legacy id and a witnessed id sharing a mined id never collide, because
        // the record's variant tag distinguishes them.
        assert_ne!(
            replica_digest(&[txid(1)].into_iter().collect()),
            replica_digest(&[wtxid(1)].into_iter().collect())
        );

        // The empty verified set is deterministic.
        assert_eq!(
            replica_digest(&HashSet::new()),
            replica_digest(&HashSet::new())
        );
    }

    #[test]
    fn replica_digest_format_snapshot() {
        // A stable, documented digest for a fixed verified set, so wire-format
        // changes are caught (the follower in a separate process recomputes this).
        let verified: HashSet<_> = [txid(1), wtxid(2)].into_iter().collect();

        let digest = replica_digest(&verified);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();

        assert_eq!(
            hex,
            "d9b1ba257021506dc586dfd532a6bf89874644dc120cf2d2871215adf00c05b1"
        );
    }

    #[test]
    fn mempool_batch_round_trips() {
        let change = MempoolChange::added(ids(&[1, 2]));
        let batch = MempoolBatch::event(change.clone());

        assert_eq!(batch.events(), std::slice::from_ref(&change));
        assert_eq!(batch.checksum(), None);

        let checksum = replica_digest(&ids(&[1]));
        let batch = MempoolBatch::new(vec![change.clone()], Some(checksum));
        assert_eq!(batch.checksum(), Some(checksum));
        assert_eq!(batch.into_events(), vec![change]);
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
