//! The [ZIP-317 block production algorithm](https://zips.z.cash/zip-0317#block-production).
//!
//! This is recommended algorithm, so these calculations are not consensus-critical,
//! or standardised across node implementations:
//! > it is sufficient to use floating point arithmetic to calculate the argument to `floor`
//! > when computing `size_target`, since there is no consensus requirement for this to be
//! > exactly the same between implementations.

use std::collections::{HashMap, HashSet};

use rand::{
    distributions::{Distribution, WeightedIndex},
    prelude::thread_rng,
};

use zebra_chain::{
    block::{Height, MAX_BLOCK_BYTES},
    parameters::Network,
    transaction::{self, zip317::BLOCK_UNPAID_ACTION_LIMIT, VerifiedUnminedTx},
};
use zebra_consensus::MAX_BLOCK_SIGOPS;
use zebra_node_services::mempool::TransactionDependencies;

use crate::methods::types::transaction::TransactionTemplate;

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
use crate::methods::{Amount, NonNegative};

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
use zebra_chain::parameters::NetworkUpgrade;

#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::methods::types::get_block_template::InBlockTxDependenciesDepth;

use super::MinerParams;

/// Used in the return type of [`select_mempool_transactions()`] for test compilations.
#[cfg(test)]
type SelectedMempoolTx = (InBlockTxDependenciesDepth, VerifiedUnminedTx);

/// Used in the return type of [`select_mempool_transactions()`] for non-test compilations.
#[cfg(not(test))]
type SelectedMempoolTx = VerifiedUnminedTx;

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
#[allow(clippy::too_many_arguments)]
pub fn select_mempool_transactions(
    net: &Network,
    height: Height,
    miner_params: &MinerParams,
    mempool_txs: Vec<VerifiedUnminedTx>,
    mempool_tx_deps: TransactionDependencies,
    #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))] zip233_amount: Option<
        Amount<NonNegative>,
    >,
) -> Vec<SelectedMempoolTx> {
    // Use a fake coinbase transaction to break the dependency between transaction
    // selection, the miner fee, and the fee payment in the coinbase transaction.
    let fake_coinbase_tx = TransactionTemplate::new_coinbase(
        net,
        height,
        miner_params,
        &[],
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
        zip233_amount,
    )
    .expect("valid coinbase transaction template");

    let tx_dependencies = mempool_tx_deps.dependencies();
    let (independent_mempool_txs, mut dependent_mempool_txs): (HashMap<_, _>, HashMap<_, _>) =
        mempool_txs
            .into_iter()
            .map(|tx| (tx.transaction.id.mined_id(), tx))
            .partition(|(tx_id, _tx)| !tx_dependencies.contains_key(tx_id));

    // Setup the transaction lists.
    let (mut conventional_fee_txs, mut low_fee_txs): (Vec<_>, Vec<_>) = independent_mempool_txs
        .into_values()
        .partition(VerifiedUnminedTx::pays_conventional_fee);

    let mut selected_txs = Vec::new();

    // Set up limit tracking
    let mut remaining_block_bytes: usize = MAX_BLOCK_BYTES.try_into().expect("fits in memory");
    let mut remaining_block_sigops = MAX_BLOCK_SIGOPS;
    let mut remaining_block_unpaid_actions: u32 = BLOCK_UNPAID_ACTION_LIMIT;

    // Adjust the limits based on the coinbase transaction
    remaining_block_bytes -= fake_coinbase_tx.data.as_ref().len();
    remaining_block_sigops -= fake_coinbase_tx.sigops;

    // > Repeat while there is any candidate transaction
    // > that pays at least the conventional fee:
    let mut conventional_fee_tx_weights = setup_fee_weighted_index(&conventional_fee_txs);

    while let Some(tx_weights) = conventional_fee_tx_weights {
        conventional_fee_tx_weights = checked_add_transaction_weighted_random(
            &mut conventional_fee_txs,
            &mut dependent_mempool_txs,
            tx_weights,
            &mut selected_txs,
            &mempool_tx_deps,
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
            &mut low_fee_txs,
            &mut dependent_mempool_txs,
            tx_weights,
            &mut selected_txs,
            &mempool_tx_deps,
            &mut remaining_block_bytes,
            &mut remaining_block_sigops,
            &mut remaining_block_unpaid_actions,
        );
    }

    selected_txs
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

/// Checks if every item in `candidate_tx_deps` is present in `selected_txs`.
///
/// Requires items in `selected_txs` to be unique to work correctly.
fn has_direct_dependencies(
    candidate_tx_deps: Option<&HashSet<transaction::Hash>>,
    selected_txs: &Vec<SelectedMempoolTx>,
) -> bool {
    let Some(deps) = candidate_tx_deps else {
        return true;
    };

    if selected_txs.len() < deps.len() {
        return false;
    }

    let mut num_available_deps = 0;
    for tx in selected_txs {
        #[cfg(test)]
        let (_, tx) = tx;
        if deps.contains(&tx.transaction.id.mined_id()) {
            num_available_deps += 1;
        } else {
            continue;
        }

        if num_available_deps == deps.len() {
            return true;
        }
    }

    false
}

/// Returns the depth of a transaction's dependencies in the block for a candidate
/// transaction with the provided dependencies.
#[cfg(test)]
fn dependencies_depth(
    dependent_tx_id: &transaction::Hash,
    mempool_tx_deps: &TransactionDependencies,
) -> InBlockTxDependenciesDepth {
    let mut current_level = 0;
    let mut current_level_deps = mempool_tx_deps.direct_dependencies(dependent_tx_id);
    while !current_level_deps.is_empty() {
        current_level += 1;
        current_level_deps = current_level_deps
            .iter()
            .flat_map(|dep_id| mempool_tx_deps.direct_dependencies(dep_id))
            .collect();
    }

    current_level
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
// TODO: Refactor these arguments into a struct and this function into a method.
#[allow(clippy::too_many_arguments)]
fn checked_add_transaction_weighted_random(
    candidate_txs: &mut Vec<VerifiedUnminedTx>,
    dependent_txs: &mut HashMap<transaction::Hash, VerifiedUnminedTx>,
    tx_weights: WeightedIndex<f32>,
    selected_txs: &mut Vec<SelectedMempoolTx>,
    mempool_tx_deps: &TransactionDependencies,
    remaining_block_bytes: &mut usize,
    remaining_block_sigops: &mut u32,
    remaining_block_unpaid_actions: &mut u32,
) -> Option<WeightedIndex<f32>> {
    // > Pick one of those transactions at random with probability in direct proportion
    // > to its weight_ratio, and remove it from the set of candidate transactions
    let (new_tx_weights, candidate_tx) =
        choose_transaction_weighted_random(candidate_txs, tx_weights);

    if !candidate_tx.try_update_block_template_limits(
        remaining_block_bytes,
        remaining_block_sigops,
        remaining_block_unpaid_actions,
    ) {
        return new_tx_weights;
    }

    let tx_dependencies = mempool_tx_deps.dependencies();
    let selected_tx_id = &candidate_tx.transaction.id.mined_id();
    debug_assert!(
        !tx_dependencies.contains_key(selected_tx_id),
        "all candidate transactions should be independent"
    );

    #[cfg(not(test))]
    selected_txs.push(candidate_tx);

    #[cfg(test)]
    selected_txs.push((0, candidate_tx));

    // Try adding any dependent transactions if all of their dependencies have been selected.

    let mut current_level_dependents = mempool_tx_deps.direct_dependents(selected_tx_id);
    while !current_level_dependents.is_empty() {
        let mut next_level_dependents = HashSet::new();

        for dependent_tx_id in &current_level_dependents {
            // ## Note
            //
            // A necessary condition for adding the dependent tx is that it spends unmined outputs coming only from
            // the selected txs, which come from the mempool. If the tx also spends in-chain outputs, it won't
            // be added. This behavior is not specified by consensus rules and can be changed at any time,
            // meaning that such txs could be added.
            if has_direct_dependencies(tx_dependencies.get(dependent_tx_id), selected_txs) {
                let Some(candidate_tx) = dependent_txs.remove(dependent_tx_id) else {
                    continue;
                };

                // Transactions that don't pay the conventional fee should not have
                // the same probability of being included as their dependencies.
                if !candidate_tx.pays_conventional_fee() {
                    continue;
                }

                if !candidate_tx.try_update_block_template_limits(
                    remaining_block_bytes,
                    remaining_block_sigops,
                    remaining_block_unpaid_actions,
                ) {
                    continue;
                }

                #[cfg(not(test))]
                selected_txs.push(candidate_tx);

                #[cfg(test)]
                selected_txs.push((
                    dependencies_depth(dependent_tx_id, mempool_tx_deps),
                    candidate_tx,
                ));

                next_level_dependents.extend(mempool_tx_deps.direct_dependents(dependent_tx_id));
            }
        }

        current_level_dependents = next_level_dependents;
    }

    new_tx_weights
}

trait TryUpdateBlockLimits {
    /// Checks if a transaction fits within the provided remaining block bytes,
    /// sigops, and unpaid actions limits.
    ///
    /// Updates the limits and returns true if the transaction does fit, or
    /// returns false otherwise.
    fn try_update_block_template_limits(
        &self,
        remaining_block_bytes: &mut usize,
        remaining_block_sigops: &mut u32,
        remaining_block_unpaid_actions: &mut u32,
    ) -> bool;
}

impl TryUpdateBlockLimits for VerifiedUnminedTx {
    fn try_update_block_template_limits(
        &self,
        remaining_block_bytes: &mut usize,
        remaining_block_sigops: &mut u32,
        remaining_block_unpaid_actions: &mut u32,
    ) -> bool {
        // > If the block template with this transaction included
        // > would be within the block size limit and block sigop limit,
        // > and block_unpaid_actions <=  block_unpaid_action_limit,
        // > add the transaction to the block template
        //
        // Unpaid actions are always zero for transactions that pay the conventional fee,
        // so the unpaid action check always passes for those transactions.
        if self.transaction.size <= *remaining_block_bytes
            && self.sigops <= *remaining_block_sigops
            && self.unpaid_actions <= *remaining_block_unpaid_actions
        {
            *remaining_block_bytes -= self.transaction.size;
            *remaining_block_sigops -= self.sigops;

            // Unpaid actions are always zero for transactions that pay the conventional fee,
            // so this limit always remains the same after they are added.
            *remaining_block_unpaid_actions -= self.unpaid_actions;

            true
        } else {
            false
        }
    }
}

/// Choose a transaction from `transactions`, using the previously set up `weighted_index`.
///
/// If some transactions have not yet been chosen, returns the weighted index and the transaction.
/// Otherwise, just returns the transaction.
fn choose_transaction_weighted_random(
    candidate_txs: &mut Vec<VerifiedUnminedTx>,
    weighted_index: WeightedIndex<f32>,
) -> (Option<WeightedIndex<f32>>, VerifiedUnminedTx) {
    let candidate_position = weighted_index.sample(&mut thread_rng());
    let candidate_tx = candidate_txs.swap_remove(candidate_position);

    // We have to regenerate this index each time we choose a transaction, due to floating-point sum inaccuracies.
    // If we don't, some chosen transactions can end up with a tiny non-zero weight, leading to duplicates.
    // <https://github.com/rust-random/rand/blob/4bde8a0adb517ec956fcec91665922f6360f974b/src/distributions/weighted_index.rs#L173-L183>
    (setup_fee_weighted_index(candidate_txs), candidate_tx)
}
