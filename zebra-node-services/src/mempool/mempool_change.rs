//! Defines the MempoolChange enum

use std::collections::HashSet;

use zebra_chain::transaction::UnminedTxId;

/// Represents a change in the mempool's verified set of transactions
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum MempoolChange {
    /// The set of [`UnminedTxId`]s of transactions that were added to the mempool.
    Added(HashSet<UnminedTxId>),
    /// The set of [`UnminedTxId`]s of transactions that were invalidated from the
    /// mempool, or which were not added because they could not be verified.
    Invalidated(HashSet<UnminedTxId>),
    /// The set of [`UnminedTxId`]s of transactions that were removed from the
    /// mempool because they were mined onto the best chain.
    Mined(HashSet<UnminedTxId>),
}
