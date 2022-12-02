//! The [ZIP-317 block production algorithm](https://zips.z.cash/zip-0317#block-production).
//!
//! This is recommended algorithm, so these calculations are not consensus-critical,
//! or standardised across node implementations:
//! > it is sufficient to use floating point arithmetic to calculate the argument to `floor`
//! > when computing `size_target`, since there is no consensus requirement for this to be
//! > exactly the same between implementations.

use jsonrpc_core::{Error, ErrorCode, Result};
use rand::{
    distributions::{Distribution, WeightedIndex},
    prelude::thread_rng,
};
use tower::{Service, ServiceExt};

use zebra_chain::{block::MAX_BLOCK_BYTES, transaction::VerifiedUnminedTx};
use zebra_consensus::MAX_BLOCK_SIGOPS;
use zebra_node_services::mempool;

/// The ZIP-317 recommended limit on the number of unpaid actions per block.
/// `block_unpaid_action_limit` in ZIP-317.
pub const BLOCK_PRODUCTION_UNPAID_ACTION_LIMIT: u32 = 50;

/// Selects mempool transactions for block production according to [ZIP-317].
///
/// Returns selected transactions from the `mempool`, or an error if the mempool has failed.
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
pub async fn select_mempool_transactions<Mempool>(
    mempool: Mempool,
) -> Result<Vec<VerifiedUnminedTx>>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
{
    // Setup the transaction lists.
    let mempool_transactions = fetch_mempool_transactions(mempool).await?;

    let (conventional_fee_txs, low_fee_txs): (Vec<_>, Vec<_>) = mempool_transactions
        .into_iter()
        .partition(VerifiedUnminedTx::pays_conventional_fee);

    let mut selected_txs = Vec::new();

    // Set up limit tracking
    let mut remaining_block_bytes: usize = MAX_BLOCK_BYTES.try_into().expect("fits in memory");
    let mut remaining_block_sigops = MAX_BLOCK_SIGOPS;

    if let Some(conventional_fee_tx_weights) = setup_fee_weighted_index(&conventional_fee_txs) {
        let mut conventional_fee_tx_weights = Some(conventional_fee_tx_weights);

        // > Repeat while there is any candidate transaction
        // > that pays at least the conventional fee:
        while let Some(tx_weights) = conventional_fee_tx_weights {
            // > Pick one of those transactions at random with probability in direct proportion
            // > to its weight_ratio, and remove it from the set of candidate transactions
            let (tx_weights, candidate_tx) =
                choose_transaction_weighted_random(&conventional_fee_txs, tx_weights);
            conventional_fee_tx_weights = tx_weights;

            // > If the block template with this transaction included
            // > would be within the block size limit and block sigop limit,
            // > add the transaction to the block template
            if candidate_tx.transaction.size <= remaining_block_bytes
                && candidate_tx.legacy_sigop_count <= remaining_block_sigops
            {
                selected_txs.push(candidate_tx.clone());

                remaining_block_bytes -= candidate_tx.transaction.size;
                remaining_block_sigops -= candidate_tx.legacy_sigop_count;
            }
        }
    }

    // Set up limit tracking
    let mut remaining_block_unpaid_actions: u32 = BLOCK_PRODUCTION_UNPAID_ACTION_LIMIT;

    // > Repeat while there is any candidate transaction
    if let Some(low_fee_tx_weights) = setup_fee_weighted_index(&low_fee_txs) {
        let mut low_fee_tx_weights = Some(low_fee_tx_weights);

        while let Some(tx_weights) = low_fee_tx_weights {
            // > Pick one of those transactions at random with probability in direct proportion
            // > to its weight_ratio, and remove it from the set of candidate transactions
            let (tx_weights, candidate_tx) =
                choose_transaction_weighted_random(&low_fee_txs, tx_weights);
            low_fee_tx_weights = tx_weights;

            // > If the block template with this transaction included
            // > would be within the block size limit and block sigop limit,
            // > and block_unpaid_actions <=  block_unpaid_action_limit,
            // > add the transaction to the block template
            if candidate_tx.transaction.size <= remaining_block_bytes
                && candidate_tx.legacy_sigop_count <= remaining_block_sigops
                && candidate_tx.unpaid_actions <= remaining_block_unpaid_actions
            {
                selected_txs.push(candidate_tx.clone());

                remaining_block_bytes -= candidate_tx.transaction.size;
                remaining_block_sigops -= candidate_tx.legacy_sigop_count;
                remaining_block_unpaid_actions -= candidate_tx.unpaid_actions;
            }
        }
    }

    Ok(selected_txs)
}

/// Fetch the transactions that are currently in `mempool`.
async fn fetch_mempool_transactions<Mempool>(mempool: Mempool) -> Result<Vec<VerifiedUnminedTx>>
where
    Mempool: Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
{
    let response = mempool
        .oneshot(mempool::Request::FullTransactions)
        .await
        .map_err(|error| Error {
            code: ErrorCode::ServerError(0),
            message: error.to_string(),
            data: None,
        })?;

    if let mempool::Response::FullTransactions(transactions) = response {
        Ok(transactions)
    } else {
        unreachable!("unmatched response to a mempool::FullTransactions request")
    }
}

/// Returns a fee-weighted index and the total weight of `transactions`.
///
/// Returns `None` if there are no transactions, or if the weights are invalid.
fn setup_fee_weighted_index(transactions: &[VerifiedUnminedTx]) -> Option<WeightedIndex<f32>> {
    if transactions.is_empty() {
        return None;
    }

    let tx_weights: Vec<f32> = transactions.iter().map(|tx| tx.fee_weight_ratio).collect();

    // Setup the transaction weights.
    WeightedIndex::new(tx_weights).ok()
}

/// Choose a transaction from `transactions`, using the previously set up `weighted_index`.
///
/// If some transactions have not yet been chosen, returns the weighted index and the transaction.
/// Otherwise, just returns the transaction.
fn choose_transaction_weighted_random(
    transactions: &[VerifiedUnminedTx],
    mut weighted_index: WeightedIndex<f32>,
) -> (Option<WeightedIndex<f32>>, VerifiedUnminedTx) {
    let candidate_position = weighted_index.sample(&mut thread_rng());
    let candidate_tx = transactions[candidate_position].clone();

    // Only pick each transaction once, by setting picked transaction weights to zero
    if weighted_index
        .update_weights(&[(candidate_position, &0.0)])
        .is_err()
    {
        // All weights are zero, so each transaction has either been selected or rejected
        (None, candidate_tx)
    } else {
        (Some(weighted_index), candidate_tx)
    }
}
