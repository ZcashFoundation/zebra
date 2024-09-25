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
    amount::NegativeOrZero,
    block::{Height, MAX_BLOCK_BYTES},
    parameters::Network,
    transaction::{self, zip317::BLOCK_UNPAID_ACTION_LIMIT, VerifiedUnminedTx},
    transparent,
};
use zebra_consensus::MAX_BLOCK_SIGOPS;

use crate::methods::get_block_template_rpcs::{
    get_block_template::generate_coinbase_transaction, types::transaction::TransactionTemplate,
};

use super::get_block_template::InBlockTxDependenciesDepth;

/// Selects mempool transactions for block production according to [ZIP-317],
/// using a fake coinbase transaction and the mempool.
///
/// The fake coinbase transaction's serialized size and sigops must be at least as large
/// as the real coinbase transaction. (The real coinbase transaction depends on the total
/// fees from the transactions returned by this function.)
/// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
/// in the `getblocktemplate` RPC.
///
/// Returns selected transactions from `mempool_txs`.
///
/// [ZIP-317]: https://zips.z.cash/zip-0317#block-production
pub fn select_mempool_transactions(
    network: &Network,
    next_block_height: Height,
    miner_address: &transparent::Address,
    mempool_txs: Vec<VerifiedUnminedTx>,
    mempool_tx_deps: &HashMap<transaction::Hash, HashSet<transaction::Hash>>,
    like_zcashd: bool,
    extra_coinbase_data: Vec<u8>,
) -> Vec<(InBlockTxDependenciesDepth, VerifiedUnminedTx)> {
    // Use a fake coinbase transaction to break the dependency between transaction
    // selection, the miner fee, and the fee payment in the coinbase transaction.
    let fake_coinbase_tx = fake_coinbase_transaction(
        network,
        next_block_height,
        miner_address,
        like_zcashd,
        extra_coinbase_data,
    );

    // Setup the transaction lists.
    let (mut conventional_fee_txs, mut low_fee_txs): (Vec<_>, Vec<_>) = mempool_txs
        .into_iter()
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
            tx_weights,
            &mut selected_txs,
            mempool_tx_deps,
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
            tx_weights,
            &mut selected_txs,
            mempool_tx_deps,
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
///
/// If `like_zcashd` is true, try to match the coinbase transactions generated by `zcashd`
/// in the `getblocktemplate` RPC.
pub fn fake_coinbase_transaction(
    network: &Network,
    height: Height,
    miner_address: &transparent::Address,
    like_zcashd: bool,
    extra_coinbase_data: Vec<u8>,
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

    let coinbase_tx = generate_coinbase_transaction(
        network,
        height,
        miner_address,
        miner_fee,
        like_zcashd,
        extra_coinbase_data,
    );

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

/// Checks if every item in `candidate_tx_deps` is present in `selected_txs`.
///
/// Requires items in `selected_txs` to be unique to work correctly.
fn has_direct_dependencies(
    candidate_tx_deps: Option<&HashSet<transaction::Hash>>,
    selected_txs: &Vec<(InBlockTxDependenciesDepth, VerifiedUnminedTx)>,
) -> bool {
    let Some(deps) = candidate_tx_deps else {
        return true;
    };

    let mut num_available_deps = 0;
    for (_, tx) in selected_txs {
        if deps.contains(&tx.transaction.id.mined_id()) {
            num_available_deps += 1;
        }
    }

    num_available_deps == deps.len()
}

/// Returns the depth of a transaction's dependencies in the block for a candidate
/// transaction with the provided dependencies.
fn dependencies_depth(
    candidate_tx_deps: Option<&HashSet<transaction::Hash>>,
    mempool_tx_deps: &HashMap<transaction::Hash, HashSet<transaction::Hash>>,
) -> InBlockTxDependenciesDepth {
    let mut current_level_deps = candidate_tx_deps.cloned().unwrap_or_default();
    let mut current_level = 0;

    while !current_level_deps.is_empty() {
        current_level += 1;
        current_level_deps = current_level_deps
            .iter()
            .flat_map(|dep| mempool_tx_deps.get(dep).cloned().unwrap_or_default())
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
    tx_weights: WeightedIndex<f32>,
    selected_txs: &mut Vec<(InBlockTxDependenciesDepth, VerifiedUnminedTx)>,
    mempool_tx_deps: &HashMap<transaction::Hash, HashSet<transaction::Hash>>,
    remaining_block_bytes: &mut usize,
    remaining_block_sigops: &mut u64,
    remaining_block_unpaid_actions: &mut u32,
) -> Option<WeightedIndex<f32>> {
    // > Pick one of those transactions at random with probability in direct proportion
    // > to its weight_ratio, and remove it from the set of candidate transactions
    let (new_tx_weights, candidate_tx) =
        choose_transaction_weighted_random(candidate_txs, tx_weights);

    let candidate_tx_deps = mempool_tx_deps.get(&candidate_tx.transaction.id.mined_id());

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
        // # Correctness
        //
        // Transactions that spend outputs created in the same block
        // must appear after the transaction that created those outputs
        // 
        // TODO: If it gets here but the dependencies aren't selected, add it to a list of transactions
        //       to be added immediately if there's room once their dependencies have been selected.
        //       Unlike the other checks in this if statement, candidate transactions that fail this
        //       check may pass it in the next round, but are currently removed from the candidate set
        //       and will not be re-considered.
        && has_direct_dependencies(
            candidate_tx_deps,
            selected_txs,
        )
    {
        selected_txs.push((
            dependencies_depth(candidate_tx_deps, mempool_tx_deps),
            candidate_tx.clone(),
        ));

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

#[test]
fn excludes_tx_with_unselected_dependencies() {
    use zebra_chain::{
        amount::Amount, block::Block, serialization::ZcashDeserializeInto, transaction::UnminedTx,
    };

    let network = Network::Mainnet;
    let next_block_height = Height(1_000_000);
    let miner_address = transparent::Address::from_pub_key_hash(network.kind(), [0; 20]);
    let mut mempool_txns: Vec<_> = network
        .block_iter()
        .map(|(_, block)| {
            block
                .zcash_deserialize_into::<Block>()
                .expect("block test vector is structurally valid")
        })
        .flat_map(|block| block.transactions)
        .map(UnminedTx::from)
        // Skip transactions that fail ZIP-317 mempool checks
        .filter_map(|transaction| {
            VerifiedUnminedTx::new(
                transaction,
                Amount::try_from(1_000_000).expect("invalid value"),
                0,
            )
            .ok()
        })
        .take(2)
        .collect();

    let dep_tx_id = mempool_txns
        .pop()
        .expect("should not be empty")
        .transaction
        .id
        .mined_id();

    let mut mempool_tx_deps = HashMap::new();

    mempool_tx_deps.insert(
        mempool_txns
            .first()
            .expect("should not be empty")
            .transaction
            .id
            .mined_id(),
        [dep_tx_id].into(),
    );

    let like_zcashd = true;
    let extra_coinbase_data = Vec::new();

    assert!(
        select_mempool_transactions(
            &network,
            next_block_height,
            &miner_address,
            mempool_txns,
            &mempool_tx_deps,
            like_zcashd,
            extra_coinbase_data,
        )
        .is_empty(),
        "should not select any transactions when dependencies are unavailable"
    );
}
