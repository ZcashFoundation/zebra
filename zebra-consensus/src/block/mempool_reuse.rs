//! The block verifier's reuse of the mempool's verification of a block's transactions.

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use tower::{util::BoxCloneService, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    transaction::{self, UnminedTxId, VerifiedUnminedTx, WtxId},
};
use zebra_node_services::mempool;

use crate::{error::TransactionError, transaction as tx, BoxError};

/// Cache availability must not hold up block verification.
const MEMPOOL_REUSE_TIMEOUT: Duration = Duration::from_millis(10);

/// Asks `mempool` for its verified transactions in `block`, and returns them by mined ID.
pub(super) async fn lookup(
    mempool: Option<BoxCloneService<mempool::Request, mempool::Response, BoxError>>,
    block: &Block,
    transaction_hashes: &Arc<[transaction::Hash]>,
) -> HashMap<transaction::Hash, VerifiedUnminedTx> {
    // Query only this block's candidates, against the mempool's processed tip.
    if let Some(mempool) = mempool {
        match tokio::time::timeout(
            MEMPOOL_REUSE_TIMEOUT,
            mempool.oneshot(mempool::Request::VerifiedTransactionsForBlock {
                transaction_hashes: transaction_hashes.clone(),
                parent_hash: block.header.previous_block_hash,
            }),
        )
        .await
        {
            Ok(Ok(mempool::Response::VerifiedTransactionsForBlock(transactions))) => transactions,
            _ => HashMap::new(),
        }
    } else {
        HashMap::new()
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

/// Removes the `candidates` that the block can't reuse, and checks the block context rules of the
/// others, which the mempool's verification doesn't cover.
///
/// # Errors
///
/// Returns the error of the first candidate, in block order, that breaks a rule that depends on
/// the block. The transaction verifier returns the same error for it.
pub(super) fn recheck_candidates(
    mut candidates: HashMap<transaction::Hash, VerifiedUnminedTx>,
    block: &Block,
    transaction_hashes: &[transaction::Hash],
    rules: &ReuseRules,
) -> Result<HashMap<transaction::Hash, VerifiedUnminedTx>, TransactionError> {
    for (&hash, transaction) in transaction_hashes.iter().zip(&block.transactions) {
        let Some(candidate) = candidates.get(&hash) else {
            continue;
        };
        if transaction.is_coinbase() || !verified_in(candidate, &rules.epoch) {
            candidates.remove(&hash);
            continue;
        }
        let id = match transaction.auth_digest() {
            None => UnminedTxId::Legacy(hash),
            Some(auth_digest) => UnminedTxId::Witnessed(WtxId {
                id: hash,
                auth_digest,
            }),
        };
        if id != candidate.transaction.id {
            candidates.remove(&hash);
            continue;
        }
        // Cached proofs and scripts do not establish expiry or block lock time.
        tx::check_block_context_rules(transaction, &rules.block, &rules.network)?;
    }
    Ok::<_, TransactionError>(candidates)
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
