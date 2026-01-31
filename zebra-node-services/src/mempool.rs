//! The Zebra mempool.
//!
//! A service that manages known unmined Zcash transactions.

use std::collections::HashSet;

use tokio::sync::oneshot;
use zebra_chain::{
    block,
    transaction::{self, UnminedTx, UnminedTxId, VerifiedUnminedTx},
    transparent,
};

use crate::BoxError;

mod gossip;
mod mempool_change;
mod service_trait;
mod transaction_dependencies;

pub use self::{
    gossip::Gossip,
    mempool_change::{MempoolChange, MempoolChangeKind, MempoolTxSubscriber},
    service_trait::MempoolService,
    transaction_dependencies::TransactionDependencies,
};

/// A mempool service request.
///
/// Requests can query the current set of mempool transactions,
/// queue transactions to be downloaded and verified, or
/// run the mempool to check for newly verified transactions.
///
/// Requests can't modify the mempool directly,
/// because all mempool transactions must be verified.
#[derive(Debug, Eq, PartialEq)]
pub enum Request {
    /// Query all [`UnminedTxId`]s in the mempool.
    TransactionIds,

    /// Query matching [`UnminedTx`] in the mempool,
    /// using a unique set of [`UnminedTxId`]s.
    TransactionsById(HashSet<UnminedTxId>),

    /// Query matching [`UnminedTx`] in the mempool,
    /// using a unique set of [`transaction::Hash`]es. Pre-V5 transactions are matched
    /// directly; V5 transaction are matched just by the Hash, disregarding
    /// the [`AuthDigest`](zebra_chain::transaction::AuthDigest).
    TransactionsByMinedId(HashSet<transaction::Hash>),

    /// Request a [`transparent::Output`] identified by the given [`OutPoint`](transparent::OutPoint),
    /// waiting until it becomes available if it is unknown.
    ///
    /// This request is purely informational, and there are no guarantees about
    /// whether the UTXO remains unspent or is on the best chain, or any chain.
    /// Its purpose is to allow orphaned mempool transaction verification.
    ///
    /// # Correctness
    ///
    /// Output requests should be wrapped in a timeout, so that
    /// out-of-order and invalid requests do not hang indefinitely.
    ///
    /// Outdated requests are pruned on a regular basis.
    AwaitOutput(transparent::OutPoint),

    /// Request a [`VerifiedUnminedTx`] and its dependencies by its mined id.
    TransactionWithDepsByMinedId(transaction::Hash),

    /// Get all the [`VerifiedUnminedTx`] in the mempool.
    ///
    /// Equivalent to `TransactionsById(TransactionIds)`,
    /// but each transaction also includes the `miner_fee` and `legacy_sigop_count` fields.
    //
    // TODO: make the Transactions response return VerifiedUnminedTx,
    //       and remove the FullTransactions variant
    FullTransactions,

    /// Query matching cached rejected transaction IDs in the mempool,
    /// using a unique set of [`UnminedTxId`]s.
    RejectedTransactionIds(HashSet<UnminedTxId>),

    /// Queue a list of gossiped transactions or transaction IDs, or
    /// crawled transaction IDs.
    ///
    /// The transaction downloader checks for duplicates across IDs and transactions.
    Queue(Vec<Gossip>),

    /// Check for newly verified transactions.
    ///
    /// The transaction downloader does not push transactions into the mempool.
    /// So a task should send this request regularly (every 5-10 seconds).
    ///
    /// These checks also happen for other request variants,
    /// but we can't rely on peers to send queries regularly,
    /// and crawler queue requests depend on peer responses.
    /// Also, crawler requests aren't frequent enough for transaction propagation.
    ///
    /// # Correctness
    ///
    /// This request is required to avoid hangs in the mempool.
    ///
    /// The queue checker task can't call `poll_ready` directly on the mempool
    /// service, because the service is wrapped in a `Buffer`. Calling
    /// `Buffer::poll_ready` reserves a buffer slot, which can cause hangs
    /// when too many slots are reserved but unused:
    /// <https://docs.rs/tower/0.4.10/tower/buffer/struct.Buffer.html#a-note-on-choosing-a-bound>
    CheckForVerifiedTransactions,

    /// Request summary statistics from the mempool for `getmempoolinfo`.
    QueueStats,

    /// Check whether a transparent output is spent in the mempool.
    UnspentOutput(transparent::OutPoint),
}

/// A response to a mempool service request.
///
/// Responses can read the current set of mempool transactions,
/// check the queued status of transactions to be downloaded and verified, or
/// confirm that the mempool has been checked for newly verified transactions.
#[derive(Debug)]
pub enum Response {
    /// Returns all [`UnminedTxId`]s from the mempool.
    TransactionIds(HashSet<UnminedTxId>),

    /// Returns matching [`UnminedTx`] from the mempool.
    ///
    /// Since the [`Request::TransactionsById`] request is unique,
    /// the response transactions are also unique. The same applies to
    /// [`Request::TransactionsByMinedId`] requests, since the mempool does not allow
    /// different transactions with different mined IDs.
    Transactions(Vec<UnminedTx>),

    /// Response to [`Request::AwaitOutput`] with the transparent output
    UnspentOutput(transparent::Output),

    /// Response to [`Request::TransactionWithDepsByMinedId`].
    TransactionWithDeps {
        /// The queried transaction
        transaction: VerifiedUnminedTx,
        /// A list of dependencies of the queried transaction.
        dependencies: HashSet<transaction::Hash>,
    },

    /// Returns all [`VerifiedUnminedTx`] in the mempool.
    //
    // TODO: make the Transactions response return VerifiedUnminedTx,
    //       and remove the FullTransactions variant
    FullTransactions {
        /// All [`VerifiedUnminedTx`]s in the mempool
        transactions: Vec<VerifiedUnminedTx>,

        /// All transaction dependencies in the mempool
        transaction_dependencies: TransactionDependencies,

        /// Last seen chain tip hash by mempool service
        last_seen_tip_hash: zebra_chain::block::Hash,
    },

    /// Returns matching cached rejected [`UnminedTxId`]s from the mempool,
    RejectedTransactionIds(HashSet<UnminedTxId>),

    /// Returns a list of initial queue checks results and a oneshot receiver
    /// for awaiting download and/or verification results.
    ///
    /// Each result matches the request at the corresponding vector index.
    Queued(Vec<Result<oneshot::Receiver<Result<(), BoxError>>, BoxError>>),

    /// Confirms that the mempool has checked for recently verified transactions.
    CheckedForVerifiedTransactions,

    /// Summary statistics for the mempool: count, total size, memory usage, and regtest info.
    QueueStats {
        /// Number of transactions currently in the mempool
        size: usize,
        /// Total size in bytes of all transactions
        bytes: usize,
        /// Estimated memory usage in bytes
        usage: usize,
        /// Whether all transactions have been fully notified (regtest only)
        fully_notified: Option<bool>,
    },

    /// Returns whether a transparent output is created or spent in the mempool, if present.
    TransparentOutput(Option<CreatedOrSpent>),
}

/// Indicates whether an output was created or spent by a mempool transaction.
#[derive(Debug)]
pub enum CreatedOrSpent {
    /// An unspent output that was created by a transaction in the mempool and not spent by any other mempool tx.
    Created {
        /// The output
        output: transparent::Output,
        /// The version
        tx_version: u32,
        /// The last seen hash
        last_seen_hash: block::Hash,
    },
    /// Indicates that an output was spent by a mempool transaction.
    Spent,
}
