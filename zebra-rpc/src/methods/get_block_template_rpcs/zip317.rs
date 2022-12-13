//! The [ZIP-317 block production algorithm](https://zips.z.cash/zip-0317#block-production).
//!
//! This is recommended algorithm, so these calculations are not consensus-critical,
//! or standardised across node implementations:
//! > it is sufficient to use floating point arithmetic to calculate the argument to `floor`
//! > when computing `size_target`, since there is no consensus requirement for this to be
//! > exactly the same between implementations.

use rand::{
    distributions::{Distribution, WeightedIndex},
    prelude::thread_rng,
};

use zebra_chain::{
    amount::NegativeOrZero,
    block::{Height, MAX_BLOCK_BYTES},
    parameters::Network,
    transaction::{Transaction, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::MAX_BLOCK_SIGOPS;

use crate::methods::get_block_template_rpcs::{
    get_block_template::standard_coinbase_outputs, types::transaction::TransactionTemplate,
};

/// The ZIP-317 recommended limit on the number of unpaid actions per block.
/// `block_unpaid_action_limit` in ZIP-317.
pub const BLOCK_PRODUCTION_UNPAID_ACTION_LIMIT: u32 = 50;

/// Selects mempool transactions for block production according to [ZIP-317],
/// using a fake coinbase transaction and the mempool.
///
/// The fake coinbase transaction's serialized size and sigops must be at least as large
/// as the real coinbase transaction. (The real coinbase transaction depends on the total
/// fees from the transactions returned by this function.)
///
/// Returns selected transactions from `mempool_txs`.
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
pub async fn select_mempool_transactions(
    network: Network,
    next_block_height: Height,
    miner_address: transparent::Address,
    mempool_txs: Vec<VerifiedUnminedTx>,
) -> Vec<VerifiedUnminedTx> {
    // Use a fake coinbase transaction to break the dependency between transaction
    // selection, the miner fee, and the fee payment in the coinbase transaction.
    let fake_coinbase_tx = fake_coinbase_transaction(network, next_block_height, miner_address);

    // Setup the transaction lists.
    let (conventional_fee_txs, low_fee_txs): (Vec<_>, Vec<_>) = mempool_txs
        .into_iter()
        .partition(VerifiedUnminedTx::pays_conventional_fee);

    let mut selected_txs = Vec::new();

    // Set up limit tracking
    let mut remaining_block_bytes: usize = MAX_BLOCK_BYTES.try_into().expect("fits in memory");
    let mut remaining_block_sigops = MAX_BLOCK_SIGOPS;
    let mut remaining_block_unpaid_actions: u32 = BLOCK_PRODUCTION_UNPAID_ACTION_LIMIT;

    // Adjust the limits based on the coinbase transaction
    remaining_block_bytes -= fake_coinbase_tx.data.as_ref().len();
    remaining_block_sigops -= fake_coinbase_tx.sigops;

    // > Repeat while there is any candidate transaction
    // > that pays at least the conventional fee:
    let mut conventional_fee_tx_weights = setup_fee_weighted_index(&conventional_fee_txs);

    while let Some(tx_weights) = conventional_fee_tx_weights {
        conventional_fee_tx_weights = checked_add_transaction_weighted_random(
            &conventional_fee_txs,
            tx_weights,
            &mut selected_txs,
            &mut remaining_block_bytes,
            &mut remaining_block_sigops,
            // The number of unpaid actions is always zero for transactions that pay the
            // conventional fee, so this check and limit is effectively ignored.
            &mut remaining_block_unpaid_actions,
        );
    }

    // > Repeat while there is any candidate transaction:
    let mut low_fee_tx_weights = setup_fee_weighted_index(&low_fee_txs);

    while let Some(tx_weights) = low_fee_tx_weights {
        low_fee_tx_weights = checked_add_transaction_weighted_random(
            &low_fee_txs,
            tx_weights,
            &mut selected_txs,
            &mut remaining_block_bytes,
            &mut remaining_block_sigops,
            &mut remaining_block_unpaid_actions,
        );
    }

    selected_txs
}

/// Returns a fake coinbase transaction that can be used during transaction selection.
///
/// This avoids a data dependency loop involving the selected transactions, the miner fee,
/// and the coinbase transaction.
///
/// This transaction's serialized size and sigops must be at least as large as the real coinbase
/// transaction with the correct height and fee.
pub fn fake_coinbase_transaction(
    network: Network,
    block_height: Height,
    miner_address: transparent::Address,
) -> TransactionTemplate<NegativeOrZero> {
    // Block heights are encoded as variable-length (script) and `u32` (lock time, expiry height).
    // They can also change the `u32` consensus branch id.
    // We use the template height here, which has the correct byte length.
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    // https://github.com/zcash/zips/blob/main/zip-0203.rst#changes-for-nu5
    //
    // Transparent amounts are encoded as `i64`,
    // so one zat has the same size as the real amount:
    // https://developer.bitcoin.org/reference/transactions.html#txout-a-transaction-output
    let miner_fee = 1.try_into().expect("amount is valid and non-negative");

    let outputs = standard_coinbase_outputs(network, block_height, miner_address, miner_fee);
    let coinbase_tx = Transaction::new_v5_coinbase(network, block_height, outputs).into();

    TransactionTemplate::from_coinbase(&coinbase_tx, miner_fee)
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

/// Chooses a random transaction from `txs` using the weighted index `tx_weights`,
/// and tries to add it to `selected_txs`.
///
/// If it fits in the supplied limits, adds it to `selected_txs`, and updates the limits.
///
/// Updates the weights of chosen transactions to zero, even if they weren't added,
/// so they can't be chosen again.
///
/// Returns the updated transaction weights.
/// If all transactions have been chosen, returns `None`.
fn checked_add_transaction_weighted_random(
    candidate_txs: &[VerifiedUnminedTx],
    tx_weights: WeightedIndex<f32>,
    selected_txs: &mut Vec<VerifiedUnminedTx>,
    remaining_block_bytes: &mut usize,
    remaining_block_sigops: &mut u64,
    remaining_block_unpaid_actions: &mut u32,
) -> Option<WeightedIndex<f32>> {
    // > Pick one of those transactions at random with probability in direct proportion
    // > to its weight_ratio, and remove it from the set of candidate transactions
    let (new_tx_weights, candidate_tx) =
        choose_transaction_weighted_random(candidate_txs, tx_weights);

    // > If the block template with this transaction included
    // > would be within the block size limit and block sigop limit,
    // > and block_unpaid_actions <=  block_unpaid_action_limit,
    // > add the transaction to the block template
    //
    // Unpaid actions are always zero for transactions that pay the conventional fee,
    // so the unpaid action check always passes for those transactions.
    if candidate_tx.transaction.size <= *remaining_block_bytes
        && candidate_tx.legacy_sigop_count <= *remaining_block_sigops
        && candidate_tx.unpaid_actions <= *remaining_block_unpaid_actions
    {
        selected_txs.push(candidate_tx.clone());

        *remaining_block_bytes -= candidate_tx.transaction.size;
        *remaining_block_sigops -= candidate_tx.legacy_sigop_count;

        // Unpaid actions are always zero for transactions that pay the conventional fee,
        // so this limit always remains the same after they are added.
        *remaining_block_unpaid_actions -= candidate_tx.unpaid_actions;
    }

    new_tx_weights
}

/// Choose a transaction from `transactions`, using the previously set up `weighted_index`.
///
/// If some transactions have not yet been chosen, returns the weighted index and the transaction.
/// Otherwise, just returns the transaction.
fn choose_transaction_weighted_random(
    candidate_txs: &[VerifiedUnminedTx],
    mut weighted_index: WeightedIndex<f32>,
) -> (Option<WeightedIndex<f32>>, VerifiedUnminedTx) {
    let candidate_position = weighted_index.sample(&mut thread_rng());
    let candidate_tx = candidate_txs[candidate_position].clone();

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
