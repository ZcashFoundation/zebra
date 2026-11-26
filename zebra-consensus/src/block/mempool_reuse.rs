//! The block verifier's reuse of the mempool's verification of a block's transactions.

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use tower::{util::BoxCloneService, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    transaction::{self, Transaction, VerifiedUnminedTx},
};
use zebra_node_services::mempool;

use crate::{error::TransactionError, transaction as tx, BoxError};

/// The mempool service that the block verifier asks for its verified transactions.
pub(super) type Mempool = BoxCloneService<mempool::Request, mempool::Response, BoxError>;

/// How long the block verifier waits for the mempool's verified transactions for a block.
///
/// Without an answer in time, or with an error, the block verifier verifies all the block's
/// transactions itself, so a slow or busy mempool delays a block's verification by about this
/// long.
pub(super) const MEMPOOL_REUSE_TIMEOUT: Duration = Duration::from_millis(10);

/// How a block's mempool lookup ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lookup {
    /// The block verifier has no mempool, or the block has only a coinbase transaction.
    NotSent,
    /// The mempool answered.
    Answered,
    /// The mempool returned an error.
    Failed,
    /// The mempool didn't answer within [`MEMPOOL_REUSE_TIMEOUT`].
    TimedOut,
}

/// Asks `mempool` for its verifications of `block`'s transactions, and returns them as the
/// block's candidates. `block_hash` is the block's hash, for logs.
///
/// It waits for the answer for at most [`MEMPOOL_REUSE_TIMEOUT`]. Without an answer, or with an
/// error, the candidates are empty, so the block's transactions are verified like any other
/// block's.
pub(super) async fn lookup(
    mempool: Option<Mempool>,
    block: &Block,
    transaction_hashes: &Arc<[transaction::Hash]>,
    block_hash: block::Hash,
) -> Candidates {
    // The mempool never has a coinbase transaction, so a block with only a coinbase has nothing
    // to reuse.
    let Some(mempool) = mempool.filter(|_| block.transactions.len() > 1) else {
        return Candidates::new(Lookup::NotSent, HashMap::new());
    };

    // Query only this block's candidates, against the mempool's processed tip.
    match tokio::time::timeout(
        MEMPOOL_REUSE_TIMEOUT,
        mempool.oneshot(mempool::Request::VerifiedTransactionsForBlock {
            transaction_hashes: transaction_hashes.clone(),
            parent_hash: block.header.previous_block_hash,
        }),
    )
    .await
    {
        Ok(Ok(mempool::Response::VerifiedTransactionsForBlock(verified))) => {
            Candidates::new(Lookup::Answered, verified)
        }
        Ok(Ok(_)) => unreachable!(
            "the mempool answers VerifiedTransactionsForBlock with VerifiedTransactionsForBlock"
        ),
        Ok(Err(error)) => {
            tracing::debug!(
                ?error,
                %block_hash,
                "the mempool lookup for a block failed, so the transaction verifier verifies all \
                 its transactions",
            );
            Candidates::new(Lookup::Failed, HashMap::new())
        }
        Err(_elapsed) => {
            tracing::debug!(
                %block_hash,
                "the mempool didn't answer a block's lookup in time, so the transaction verifier \
                 verifies all its transactions",
            );
            Candidates::new(Lookup::TimedOut, HashMap::new())
        }
    }
}

/// The rules that the block verifier checks again for each transaction it takes from the mempool.
pub(super) struct ReuseRules {
    /// The heights in which the mempool's verification is valid for the block.
    pub(super) epoch: tx::VerificationRulesEpoch,
    /// The block the transactions are in.
    pub(super) block: tx::BlockContext,
    /// The network the block is on.
    pub(super) network: Network,
}

impl ReuseRules {
    /// Returns the rules for a block at `height` with the header time `time` on `network`.
    pub(super) fn new(network: &Network, height: block::Height, time: DateTime<Utc>) -> Self {
        let epoch = tx::VerificationRulesEpoch::containing(network, height);
        Self {
            block: tx::BlockContext {
                height,
                time,
                network_upgrade: epoch.network_upgrade(),
            },
            epoch,
            network: network.clone(),
        }
    }
}

/// The mempool's verifications of a block's transactions, which the block verifier takes instead
/// of verifying those transactions when it can.
pub(super) struct Candidates {
    /// How the lookup ended.
    lookup: Lookup,
    /// The mempool's verified transactions, by mined ID. Empty without an answer.
    verified: HashMap<transaction::Hash, VerifiedUnminedTx>,
    /// The number of transactions that the block verifier took from the mempool.
    reused: usize,
}

impl Candidates {
    /// Returns the candidates in `verified`, from a lookup that ended as `lookup`.
    fn new(lookup: Lookup, verified: HashMap<transaction::Hash, VerifiedUnminedTx>) -> Self {
        Self {
            lookup,
            verified,
            reused: 0,
        }
    }

    /// Returns the mempool's verification of the block's `transaction`, whose mined ID is
    /// `transaction_hash`, if the block verifier takes it instead of verifying the transaction.
    ///
    /// # Errors
    ///
    /// Returns the error of a rule that depends on the block, which the mempool's verification
    /// doesn't cover. The transaction verifier returns the same error for the transaction.
    pub(super) fn take(
        &mut self,
        transaction: &Arc<Transaction>,
        transaction_hash: transaction::Hash,
        rules: &ReuseRules,
    ) -> Result<Option<&VerifiedUnminedTx>, TransactionError> {
        let Some(candidate) = self.verified.get(&transaction_hash) else {
            return Ok(None);
        };
        if !check_candidate(candidate, transaction, transaction_hash, rules)? {
            return Ok(None);
        }
        self.reused += 1;
        Ok(Some(candidate))
    }

    /// Records how the lookup ended, and how many of the block's `non_coinbase` transactions the
    /// block verifier took from the mempool.
    ///
    /// It records nothing if the lookup wasn't sent.
    pub(super) fn record(&self, non_coinbase: usize) {
        match self.lookup {
            Lookup::NotSent => return,
            Lookup::Answered => {}
            Lookup::Failed => {
                metrics::counter!("zebra.consensus.block.mempool_reuse.error").increment(1)
            }
            Lookup::TimedOut => {
                metrics::counter!("zebra.consensus.block.mempool_reuse.timeout").increment(1)
            }
        }
        let count = |n: usize| u64::try_from(n).expect("a block's transaction count fits in a u64");
        metrics::counter!("zebra.consensus.block.mempool_reuse.hit").increment(count(self.reused));
        metrics::counter!("zebra.consensus.block.mempool_reuse.miss")
            .increment(count(non_coinbase.saturating_sub(self.reused)));
    }
}

/// Returns `true` if the block verifier can take `candidate`, the mempool's verification of the
/// block's `transaction` with the mined ID `hash`, instead of verifying the transaction.
///
/// # Errors
///
/// Returns the error of a rule that depends on the block, which the mempool's verification
/// doesn't cover. The transaction verifier returns the same error for the transaction.
pub(super) fn check_candidate(
    candidate: &VerifiedUnminedTx,
    transaction: &Arc<Transaction>,
    hash: transaction::Hash,
    rules: &ReuseRules,
) -> Result<bool, TransactionError> {
    if transaction.is_coinbase() || !verified_in(candidate, &rules.epoch) {
        return Ok(false);
    }
    // The mempool verified its own copy of the transaction's authorizing data, which can differ
    // from the block's under the same mined ID. A block rebuilt from a compact announcement
    // shares the mempool's transaction objects, so their authorizing data doesn't need hashing.
    let same_authorizing_data = Arc::ptr_eq(&candidate.transaction.transaction, transaction)
        || candidate.transaction.id == tx::unmined_id(transaction, hash);
    if !same_authorizing_data {
        return Ok(false);
    }
    // Cached proofs and scripts do not establish expiry or block lock time.
    tx::check_block_context_rules(transaction, &rules.block, &rules.network)?;
    Ok(true)
}

/// Returns `true` if the mempool verified `candidate` for a height in `epoch`.
///
/// The mempool records the tip height it verified a transaction on, so it verified the
/// transaction for the height after it (see [`VerifiedUnminedTx::height`]). That height can be
/// below the block's, because the mempool keeps its verifications while its tip grows. It can
/// also be above the block's, because the mempool can add a transaction that it verified on the
/// state's best tip before it processes that tip, and that tip can be a sibling of the block. Both
/// are reusable in the same epoch: the block context rules are checked again at the block's
/// height, and the state checks the transaction's spends when it validates the block.
fn verified_in(candidate: &VerifiedUnminedTx, epoch: &tx::VerificationRulesEpoch) -> bool {
    candidate
        .height
        .and_then(|tip| tip + 1)
        .is_some_and(|height| epoch.contains(height))
}
