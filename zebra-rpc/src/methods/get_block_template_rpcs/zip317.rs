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

    // TODO: split these into separate functions?

    if !conventional_fee_txs.is_empty() {
        // Setup the transaction weights.
        let conventional_fee_tx_weights: Vec<f32> = conventional_fee_txs
            .iter()
            .map(|tx| tx.block_production_fee_weight)
            .collect();
        let mut conventional_fee_tx_weights = WeightedIndex::new(conventional_fee_tx_weights)
            .expect(
            "there is at least one weight, all weights are non-negative, and the total is positive",
        );

        // > Repeat while there is any mempool transaction that:
        // > - pays at least the conventional fee,
        // > - is within the block sigop limit, and
        // > - fits in the block...
        loop {
            // > Pick one of those transactions at random with probability in direct proportion
            // > to its weight, and add it to the block.
            let candidate_index = conventional_fee_tx_weights.sample(&mut thread_rng());
            let candidate_tx = &conventional_fee_txs[candidate_index];

            if candidate_tx.legacy_sigop_count <= remaining_block_sigops
                && candidate_tx.transaction.size <= remaining_block_bytes
            {
                selected_txs.push(candidate_tx.clone());

                remaining_block_sigops -= candidate_tx.legacy_sigop_count;
                remaining_block_bytes -= candidate_tx.transaction.size;
            }

            // Only pick each transaction once, by setting picked transaction weights to zero
            if conventional_fee_tx_weights
                .update_weights(&[(candidate_index, &0.0)])
                .is_err()
            {
                // All weights are zero, so each transaction has either been selected or rejected
                break;
            }
        }
    }

    if !low_fee_txs.is_empty() {
        // > Let `N` be the number of remaining transactions with `tx.weight < 1`.
        // > Calculate their sum of weights.
        let low_fee_tx_weights: Vec<f32> = low_fee_txs
            .iter()
            .map(|tx| tx.block_production_fee_weight)
            .collect();
        let low_fee_tx_count = low_fee_tx_weights.len() as f32;
        let remaining_weight: f32 = low_fee_tx_weights.iter().sum();

        // > Calculate `size_target = ...`
        //
        // We track the remaining bytes within our scaled quota,
        // so there is no need to actually calculate `size_target` or `size_of_block_so_far`.
        let average_remaining_weight = remaining_weight / low_fee_tx_count;

        let remaining_block_bytes =
            remaining_block_bytes as f32 * average_remaining_weight.min(1.0);
        let mut remaining_block_bytes = remaining_block_bytes as usize;

        // Setup the transaction weights.
        let mut low_fee_tx_weights = WeightedIndex::new(low_fee_tx_weights).expect(
            "there is at least one weight, all weights are non-negative, and the total is positive",
        );

        loop {
            // > Pick a transaction with probability in direct proportion to its weight
            // > and add it to the block. If that transaction would exceed the `size_target`
            // > or the block sigop limit, stop without adding it.
            let candidate_index = low_fee_tx_weights.sample(&mut thread_rng());
            let candidate_tx = &low_fee_txs[candidate_index];

            if candidate_tx.legacy_sigop_count > remaining_block_sigops
                || candidate_tx.transaction.size > remaining_block_bytes
            {
                // We've exceeded the (scaled quota) limits
                break;
            }

            selected_txs.push(candidate_tx.clone());

            remaining_block_sigops -= candidate_tx.legacy_sigop_count;
            remaining_block_bytes -= candidate_tx.transaction.size;

            // Only pick each transaction once, by setting picked transaction weights to zero
            if low_fee_tx_weights
                .update_weights(&[(candidate_index, &0.0)])
                .is_err()
            {
                // All weights are zero, so every remaining transaction has been selected
                break;
            }
        }
    }

    Ok(selected_txs)
}

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
