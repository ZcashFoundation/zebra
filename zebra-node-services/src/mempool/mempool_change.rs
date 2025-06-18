//! Defines the [`MempoolChange`] and [`MempoolChangeKind`] types used by the mempool change broadcast channel.

use std::collections::HashSet;

use tokio::sync::broadcast;
use zebra_chain::transaction::UnminedTxId;

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

/// Represents a kind of change in the mempool's verified set of transactions
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MempoolChangeKind {
    /// Transactions were added to the mempool.
    Added,
    /// Transactions were invalidated or could not be verified and were rejected from the mempool.
    Invalidated,
    /// Transactions were mined onto the best chain and removed from the mempool.
    Mined,
}

/// Represents a change in the mempool's verified set of transactions
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

    /// Returns the kind of change that occurred in the mempool.
    pub fn kind(&self) -> MempoolChangeKind {
        self.kind
    }

    /// Returns true if the kind of change that occurred in the mempool is [`MempoolChangeKind::Added`].
    pub fn is_added(&self) -> bool {
        self.kind == MempoolChangeKind::Added
    }

    /// Consumes self and returns the set of [`UnminedTxId`]s of transactions that were affected by the change.
    pub fn into_tx_ids(self) -> HashSet<UnminedTxId> {
        self.tx_ids
    }

    /// Returns a reference to the set of [`UnminedTxId`]s of transactions that were affected by the change.
    pub fn tx_ids(&self) -> &HashSet<UnminedTxId> {
        &self.tx_ids
    }

    /// Creates a new [`MempoolChange`] indicating that transactions were added to the mempool.
    pub fn added(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(MempoolChangeKind::Added, tx_ids)
    }

    /// Creates a new [`MempoolChange`] indicating that transactions were invalidated or rejected from the mempool.
    pub fn invalidated(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(MempoolChangeKind::Invalidated, tx_ids)
    }

    /// Creates a new [`MempoolChange`] indicating that transactions were mined onto the best chain and
    /// removed from the mempool.
    pub fn mined(tx_ids: HashSet<UnminedTxId>) -> Self {
        Self::new(MempoolChangeKind::Mined, tx_ids)
    }
}
