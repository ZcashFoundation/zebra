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
    let mempool_transactions = fetch_mempool_transactions(mempool).await?;

    // Setup the transaction lists.
    let (conventional_fee_txs, low_fee_txs): (Vec<_>, Vec<_>) = mempool_transactions
        .into_iter()
        .partition(VerifiedUnminedTx::pays_conventional_fee);

    // Set up limit tracking
    let mut selected_txs = Vec::new();
    let mut remaining_block_sigops = MAX_BLOCK_SIGOPS;
    let mut remaining_block_bytes: usize = MAX_BLOCK_BYTES.try_into().expect("fits in memory");

    if let Some((conventional_fee_tx_weights, _total_weight)) =
        setup_fee_weighted_index(&conventional_fee_txs)
    {
        let mut conventional_fee_tx_weights = Some(conventional_fee_tx_weights);

        // > Repeat while there is any mempool transaction that:
        // > - pays at least the conventional fee,
        // > - is within the block sigop limit, and
        // > - fits in the block...
        while let Some(tx_weights) = conventional_fee_tx_weights {
            // > Pick one of those transactions at random with probability in direct proportion
            // > to its weight, and add it to the block.
            let (tx_weights, candidate_tx) =
                choose_transaction_weighted_random(&conventional_fee_txs, tx_weights);
            conventional_fee_tx_weights = tx_weights;

            if candidate_tx.legacy_sigop_count <= remaining_block_sigops
                && candidate_tx.transaction.size <= remaining_block_bytes
            {
                selected_txs.push(candidate_tx.clone());

                remaining_block_sigops -= candidate_tx.legacy_sigop_count;
                remaining_block_bytes -= candidate_tx.transaction.size;
            }
        }
    }

    // > Let `N` be the number of remaining transactions with `tx.weight < 1`.
    // > Calculate their sum of weights.
    if let Some((low_fee_tx_weights, remaining_weight)) = setup_fee_weighted_index(&low_fee_txs) {
        let low_fee_tx_count = low_fee_txs.len() as f32;

        // > Calculate `size_target = ...`
        //
        // We track the remaining bytes within our scaled quota,
        // so there is no need to actually calculate `size_target` or `size_of_block_so_far`.
        let average_remaining_weight = remaining_weight / low_fee_tx_count;

        let remaining_block_bytes =
            remaining_block_bytes as f32 * average_remaining_weight.min(1.0);
        let mut remaining_block_bytes = remaining_block_bytes as usize;

        let mut low_fee_tx_weights = Some(low_fee_tx_weights);

        while let Some(tx_weights) = low_fee_tx_weights {
            // > Pick a transaction with probability in direct proportion to its weight...
            let (tx_weights, candidate_tx) =
                choose_transaction_weighted_random(&low_fee_txs, tx_weights);
            low_fee_tx_weights = tx_weights;

            // > and add it to the block. If that transaction would exceed the `size_target`
            // > or the block sigop limit, stop without adding it.
            if candidate_tx.legacy_sigop_count > remaining_block_sigops
                || candidate_tx.transaction.size > remaining_block_bytes
            {
                // We've exceeded the scaled quota size limit, or the absolute sigop limit
                break;
            }

            selected_txs.push(candidate_tx.clone());

            remaining_block_sigops -= candidate_tx.legacy_sigop_count;
            remaining_block_bytes -= candidate_tx.transaction.size;
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
fn setup_fee_weighted_index(
    transactions: &[VerifiedUnminedTx],
) -> Option<(WeightedIndex<f32>, f32)> {
    if transactions.is_empty() {
        return None;
    }

    let tx_weights: Vec<f32> = transactions
        .iter()
        .map(|tx| tx.block_production_fee_weight)
        .collect();
    let total_tx_weight: f32 = tx_weights.iter().sum();

    // Setup the transaction weights.
    let tx_weights = WeightedIndex::new(tx_weights).ok()?;

    Some((tx_weights, total_tx_weight))
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
