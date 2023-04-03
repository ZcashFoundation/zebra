//! The Zebra mempool.
//!
//! A service that manages known unmined Zcash transactions.

use std::collections::HashSet;

use zebra_chain::transaction::{self, UnminedTx, UnminedTxId};

#[cfg(feature = "getblocktemplate-rpcs")]
use zebra_chain::transaction::VerifiedUnminedTx;

use crate::BoxError;

mod gossip;

pub use self::gossip::Gossip;

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

    /// Get all the [`VerifiedUnminedTx`] in the mempool.
    ///
    /// Equivalent to `TransactionsById(TransactionIds)`,
    /// but each transaction also includes the `miner_fee` and `legacy_sigop_count` fields.
    //
    // TODO: make the Transactions response return VerifiedUnminedTx,
    //       and remove the FullTransactions variant
    #[cfg(feature = "getblocktemplate-rpcs")]
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

    /// Returns all [`VerifiedUnminedTx`] in the mempool.
    //
    // TODO: make the Transactions response return VerifiedUnminedTx,
    //       and remove the FullTransactions variant
    #[cfg(feature = "getblocktemplate-rpcs")]
    FullTransactions {
        /// All [`VerifiedUnminedTx`]s in the mempool
        transactions: Vec<VerifiedUnminedTx>,

        /// Last seen chain tip hash by mempool service
        last_seen_tip_hash: zebra_chain::block::Hash,
    },

    /// Returns matching cached rejected [`UnminedTxId`]s from the mempool,
    RejectedTransactionIds(HashSet<UnminedTxId>),

    /// Returns a list of queue results.
    ///
    /// These are the results of the initial queue checks.
    /// The transaction may also fail download or verification later.
    ///
    /// Each result matches the request at the corresponding vector index.
    Queued(Vec<Result<(), BoxError>>),

    /// Confirms that the mempool has checked for recently verified transactions.
    CheckedForVerifiedTransactions,
}
