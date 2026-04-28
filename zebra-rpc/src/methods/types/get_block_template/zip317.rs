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

use zcash_keys::address::Address;

use zebra_chain::{
    amount::NegativeOrZero,
    block::{Height, MAX_BLOCK_BYTES},
    parameters::{
        Network, NetworkUpgrade, GLOBAL_SHIELDED_BUDGET, ORCHARD_BLOCK_ACTION_LIMIT,
        SAPLING_BLOCK_IO_LIMIT, SPROUT_BLOCK_JOINSPLIT_LIMIT,
    },
    transaction::{self, zip317::BLOCK_UNPAID_ACTION_LIMIT, Transaction, VerifiedUnminedTx},
};
use zebra_consensus::MAX_BLOCK_SIGOPS;
use zebra_node_services::mempool::TransactionDependencies;

use crate::methods::types::transaction::TransactionTemplate;

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
use crate::methods::Amount;

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
use zebra_chain::amount::NonNegative;

#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::methods::types::get_block_template::InBlockTxDependenciesDepth;

use super::standard_coinbase_outputs;

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
    network: &Network,
    next_block_height: Height,
    miner_address: &Address,
    mempool_txs: Vec<VerifiedUnminedTx>,
    mempool_tx_deps: TransactionDependencies,
    extra_coinbase_data: Vec<u8>,
    #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))] zip233_amount: Option<
        Amount<NonNegative>,
    >,
) -> Vec<SelectedMempoolTx> {
    // Use a fake coinbase transaction to break the dependency between transaction
    // selection, the miner fee, and the fee payment in the coinbase transaction.
    let fake_coinbase_tx = fake_coinbase_transaction(
        network,
        next_block_height,
        miner_address,
        extra_coinbase_data,
        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
        zip233_amount,
    );

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
    let mut limits = BlockTemplateLimits::initial(network, next_block_height, &fake_coinbase_tx);

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
            &mut limits,
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
            &mut limits,
        );
    }

    selected_txs
}

/// Tracks the remaining capacity of a block template against every limit a
/// candidate mempool transaction might exhaust: ZIP-317 byte/sigop/unpaid-action
/// limits and, post-NU7, the per-pool shielded action limits and the global
/// shielded budget defined by the draft "Shorter Block Target Spacing" ZIP.
///
/// Pre-NU7 the shielded fields are initialised to `u32::MAX` so the new limits
/// have no effect.
struct BlockTemplateLimits {
    remaining_bytes: usize,
    remaining_sigops: u32,
    remaining_unpaid_actions: u32,
    remaining_orchard_actions: u32,
    remaining_sapling_ios: u32,
    remaining_sprout_joinsplits: u32,
    remaining_shielded_cost: u32,
}

impl BlockTemplateLimits {
    /// Returns the initial limits for a block template at `next_block_height`,
    /// already adjusted for `fake_coinbase_tx`'s contribution to the byte and
    /// sigop limits.
    fn initial(
        network: &Network,
        next_block_height: Height,
        fake_coinbase_tx: &TransactionTemplate<NegativeOrZero>,
    ) -> Self {
        let nu7_active = NetworkUpgrade::Nu7
            .activation_height(network)
            .is_some_and(|h| next_block_height >= h);

        let (orchard, sapling_io, sprout_jss, shielded_cost) = if nu7_active {
            (
                ORCHARD_BLOCK_ACTION_LIMIT,
                SAPLING_BLOCK_IO_LIMIT,
                SPROUT_BLOCK_JOINSPLIT_LIMIT,
                GLOBAL_SHIELDED_BUDGET,
            )
        } else {
            (u32::MAX, u32::MAX, u32::MAX, u32::MAX)
        };

        let max_bytes: usize = MAX_BLOCK_BYTES.try_into().expect("fits in memory");
        Self {
            remaining_bytes: max_bytes - fake_coinbase_tx.data.as_ref().len(),
            remaining_sigops: MAX_BLOCK_SIGOPS - fake_coinbase_tx.sigops,
            remaining_unpaid_actions: BLOCK_UNPAID_ACTION_LIMIT,
            remaining_orchard_actions: orchard,
            remaining_sapling_ios: sapling_io,
            remaining_sprout_joinsplits: sprout_jss,
            remaining_shielded_cost: shielded_cost,
        }
    }

    /// Tries to add `tx` to the block template. Returns `true` and decrements
    /// the remaining capacity if it fits within every limit; otherwise returns
    /// `false` and leaves `self` unchanged.
    fn try_add(&mut self, tx: &VerifiedUnminedTx) -> bool {
        let inner = &tx.transaction.transaction;
        // Per-block counts fit in u32 because the block size is bounded to 2 MB
        // and each shielded item is much larger than 4 bytes.
        let orchard_actions = inner.orchard_actions().count() as u32;
        let sapling_ios =
            (inner.sapling_spends_per_anchor().count() + inner.sapling_outputs().count()) as u32;
        let sprout_joinsplits = inner.joinsplit_count() as u32;
        let shielded_cost = orchard_actions + sapling_ios + sprout_joinsplits * 2;

        // > If the block template with this transaction included
        // > would be within the block size limit and block sigop limit,
        // > and block_unpaid_actions <= block_unpaid_action_limit,
        // > add the transaction to the block template
        //
        // Unpaid actions are always zero for transactions that pay the conventional fee,
        // so the unpaid action check always passes for those transactions.
        if tx.transaction.size > self.remaining_bytes
            || tx.legacy_sigop_count > self.remaining_sigops
            || tx.unpaid_actions > self.remaining_unpaid_actions
            || orchard_actions > self.remaining_orchard_actions
            || sapling_ios > self.remaining_sapling_ios
            || sprout_joinsplits > self.remaining_sprout_joinsplits
            || shielded_cost > self.remaining_shielded_cost
        {
            return false;
        }

        self.remaining_bytes -= tx.transaction.size;
        self.remaining_sigops -= tx.legacy_sigop_count;
        self.remaining_unpaid_actions -= tx.unpaid_actions;
        self.remaining_orchard_actions -= orchard_actions;
        self.remaining_sapling_ios -= sapling_ios;
        self.remaining_sprout_joinsplits -= sprout_joinsplits;
        self.remaining_shielded_cost -= shielded_cost;
        true
    }
}

/// Returns a fake coinbase transaction that can be used during transaction selection.
///
/// This avoids a data dependency loop involving the selected transactions, the miner fee,
/// and the coinbase transaction.
///
/// This transaction's serialized size and sigops must be at least as large as the real coinbase
/// transaction with the correct height and fee.
pub fn fake_coinbase_transaction(
    net: &Network,
    height: Height,
    miner_address: &Address,
    extra_coinbase_data: Vec<u8>,
    #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))] zip233_amount: Option<
        Amount<NonNegative>,
    >,
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
    let outputs = standard_coinbase_outputs(net, height, miner_address, miner_fee);

    #[cfg(not(all(zcash_unstable = "nu7", feature = "tx_v6")))]
    let coinbase = Transaction::new_v5_coinbase(net, height, outputs, extra_coinbase_data).into();

    #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
    let coinbase = {
        let network_upgrade = NetworkUpgrade::current(net, height);
        if network_upgrade < NetworkUpgrade::Nu7 {
            Transaction::new_v5_coinbase(net, height, outputs, extra_coinbase_data).into()
        } else {
            Transaction::new_v6_coinbase(
                net,
                height,
                outputs,
                extra_coinbase_data,
                zip233_amount,
                #[cfg(zcash_unstable = "zip235")]
                miner_fee,
            )
            .into()
        }
    };

    TransactionTemplate::from_coinbase(&coinbase, miner_fee)
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
fn checked_add_transaction_weighted_random(
    candidate_txs: &mut Vec<VerifiedUnminedTx>,
    dependent_txs: &mut HashMap<transaction::Hash, VerifiedUnminedTx>,
    tx_weights: WeightedIndex<f32>,
    selected_txs: &mut Vec<SelectedMempoolTx>,
    mempool_tx_deps: &TransactionDependencies,
    limits: &mut BlockTemplateLimits,
) -> Option<WeightedIndex<f32>> {
    // > Pick one of those transactions at random with probability in direct proportion
    // > to its weight_ratio, and remove it from the set of candidate transactions
    let (new_tx_weights, candidate_tx) =
        choose_transaction_weighted_random(candidate_txs, tx_weights);

    if !limits.try_add(&candidate_tx) {
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

                if !limits.try_add(&candidate_tx) {
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
