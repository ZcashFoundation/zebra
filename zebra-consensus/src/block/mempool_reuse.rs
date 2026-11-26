//! The block verifier's reuse of the mempool's verification of a block's transactions.

use std::{collections::HashMap, sync::Arc, time::Duration};

use tower::{util::BoxCloneService, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade},
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
    height: block::Height,
    network: &Network,
) -> Result<HashMap<transaction::Hash, VerifiedUnminedTx>, TransactionError> {
    let upgrade = NetworkUpgrade::current(network, height);
    let block_context = tx::BlockContext {
        height,
        time: block.header.time,
        network_upgrade: upgrade,
    };
    for (&hash, transaction) in transaction_hashes.iter().zip(&block.transactions) {
        let Some(candidate) = candidates.get(&hash) else {
            continue;
        };
        // Admission records the tip height only after confirming it still matches
        // the verification request. Older entries are safe only within that epoch.
        let same_epoch = candidate
            .height
            .and_then(|tip| tip + 1)
            .is_some_and(|height| NetworkUpgrade::current(network, height) == upgrade);
        if transaction.is_coinbase() || !same_epoch {
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
        tx::check_block_context_rules(transaction, &block_context, network)?;
    }
    Ok::<_, TransactionError>(candidates)
}
