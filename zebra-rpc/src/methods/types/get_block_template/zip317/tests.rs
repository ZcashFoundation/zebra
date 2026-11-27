//! Tests for transaction selection for block template production

#![allow(clippy::unwrap_in_result)]

use std::sync::Arc;

use chrono::{DateTime, Utc};
use zcash_keys::address::Address;
use zcash_transparent::address::TransparentAddress;

use zebra_chain::{
    amount::Amount,
    block::{Header, Height, MAX_BLOCK_BYTES},
    parameters::{Network, NetworkUpgrade},
    transaction::{
        self,
        arbitrary::{fake_bundle_for_branch, fake_v6_transaction},
        zip317::{conventional_actions, conventional_fee_weight_ratio, BLOCK_UNPAID_ACTION_LIMIT},
        LockTime, Transaction, UnminedTx, VerifiedUnminedTx,
    },
    transparent::OutPoint,
};
use zebra_consensus::{ShieldedActionCounts, MAX_BLOCK_SIGOPS};
use zebra_node_services::mempool::TransactionDependencies;

use crate::methods::types::{get_block_template::MinerParams, transaction::TransactionTemplate};

use super::{
    max_transaction_count_size, select_by_fee_rate, select_mempool_transactions, BlockSpace,
    ShieldedBudget,
};

#[test]
fn excludes_tx_with_unselected_dependencies() {
    let network = Network::Mainnet;
    let mut mempool_tx_deps = TransactionDependencies::default();

    let unmined_tx = network
        .unmined_transactions_in_blocks(..)
        .next()
        .expect("should not be empty");

    mempool_tx_deps.add(
        unmined_tx.transaction.id.mined_id(),
        vec![OutPoint::from_usize(transaction::Hash([0; 32]), 0)],
    );

    assert_eq!(
        select_mempool_transactions(
            &network,
            Height(1_000_000),
            &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
            vec![unmined_tx],
            mempool_tx_deps,
            None,
            None,
        ),
        vec![],
        "should not select any transactions when dependencies are unavailable"
    );
}

#[test]
fn includes_tx_with_selected_dependencies() {
    let network = Network::Mainnet;
    let unmined_txs: Vec<_> = network.unmined_transactions_in_blocks(..).take(3).collect();

    let dependent_tx1 = unmined_txs.first().expect("should have 3 txns");
    let dependent_tx2 = unmined_txs.get(1).expect("should have 3 txns");
    let independent_tx_id = unmined_txs
        .get(2)
        .expect("should have 3 txns")
        .transaction
        .id
        .mined_id();

    let mut mempool_tx_deps = TransactionDependencies::default();
    mempool_tx_deps.add(
        dependent_tx1.transaction.id.mined_id(),
        vec![OutPoint::from_usize(independent_tx_id, 0)],
    );
    mempool_tx_deps.add(
        dependent_tx2.transaction.id.mined_id(),
        vec![
            OutPoint::from_usize(independent_tx_id, 0),
            OutPoint::from_usize(transaction::Hash([0; 32]), 0),
        ],
    );

    let selected_txs = select_mempool_transactions(
        &network,
        Height(1_000_000),
        &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
        unmined_txs.clone(),
        mempool_tx_deps.clone(),
        None,
        None,
    );

    assert_eq!(
        selected_txs.len(),
        2,
        "should select the independent transaction and 1 of the dependent txs, selected: {selected_txs:?}"
    );

    let selected_tx_by_id = |id| {
        selected_txs
            .iter()
            .find(|(_, tx)| tx.transaction.id.mined_id() == id)
    };

    let (dependency_depth, _) =
        selected_tx_by_id(independent_tx_id).expect("should select the independent tx");

    assert_eq!(
        *dependency_depth, 0,
        "should return a dependency depth of 0 for the independent tx"
    );

    let (dependency_depth, _) = selected_tx_by_id(dependent_tx1.transaction.id.mined_id())
        .expect("should select dependent_tx1");

    assert_eq!(
        *dependency_depth, 1,
        "should return a dependency depth of 1 for the dependent tx"
    );
}

/// Checks that transaction selection reserves space for the block header and the transaction
/// count, which [`MAX_BLOCK_BYTES`] covers: a transaction exactly filling the remaining safe
/// budget is selected, and a transaction one byte larger is not (GHSA-95m2-vx53-v2jw).
#[test]
fn reserves_space_for_block_header_and_transaction_count() {
    let network = Network::Mainnet;
    let height = Height(1_000_000);
    let miner_params =
        MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20])));

    let coinbase_tx_size =
        TransactionTemplate::new_coinbase(&network, height, &miner_params, Amount::zero())
            .expect("valid coinbase transaction template")
            .data
            .as_ref()
            .len();

    let safe_budget = usize::try_from(MAX_BLOCK_BYTES).expect("fits in memory")
        - Header::serialized_size(&network)
        - max_transaction_count_size()
        - coinbase_tx_size;

    let mut unmined_tx = network
        .unmined_transactions_in_blocks(..)
        .next()
        .expect("should not be empty");

    unmined_tx.transaction.size = safe_budget;

    assert_eq!(
        select_mempool_transactions(
            &network,
            height,
            &miner_params,
            vec![unmined_tx.clone()],
            TransactionDependencies::default(),
            None,
            None,
        )
        .len(),
        1,
        "should select a transaction exactly filling the safe block budget"
    );

    unmined_tx.transaction.size = safe_budget + 1;

    assert_eq!(
        select_mempool_transactions(
            &network,
            height,
            &miner_params,
            vec![unmined_tx],
            TransactionDependencies::default(),
            None,
            None,
        ),
        vec![],
        "should not select a transaction one byte over the safe block budget"
    );
}

/// Orchard and Ironwood candidates must share the ZIP-218 budget with the Sapling coinbase.
#[test]
fn reserves_shielded_budget_for_sapling_coinbase() {
    use super::super::CoinbaseCache;
    use crate::config::mining::{default_miner_address, MinerAddressType};
    use std::sync::Arc;
    use zebra_chain::{
        parameters::{testnet::ConfiguredActivationHeights, NetworkUpgrade},
        serialization::{ZcashDeserializeInto, ZcashSerialize},
        transaction::{
            arbitrary::{fake_bundle_for_branch, fake_v6_transaction},
            Transaction, VerifiedUnminedTx,
        },
    };
    use zebra_consensus::ShieldedActionCounts;

    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(6),
            nu7: Some(1_000),
            ..Default::default()
        }
        .into(),
    );
    let height = Height(1_000);
    let miner = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Sapling),
        )
        .unwrap(),
    );
    let cache = CoinbaseCache::default();
    let sizing = TransactionTemplate::new_coinbase_with_parent_pools(
        &network,
        height,
        &miner,
        Amount::zero(),
        Some(Amount::zero()),
    )
    .unwrap();
    let sizing_tx: Transaction = sizing.data.as_ref().zcash_deserialize_into().unwrap();
    let coinbase_counts = ShieldedActionCounts::from_transaction(&sizing_tx);
    assert!(sizing_tx.sapling_outputs().count() > 0);
    cache.store(height, Amount::zero(), Some(Amount::zero()), sizing);

    let transaction = fake_v6_transaction(
        NetworkUpgrade::Nu7,
        Some(
            fake_bundle_for_branch(
                zcash_protocol::consensus::BranchId::Nu7,
                orchard::ValuePool::Orchard,
                2,
                1,
            )
            .expect("NU7 supports Orchard"),
        ),
        Some(
            fake_bundle_for_branch(
                zcash_protocol::consensus::BranchId::Nu7,
                orchard::ValuePool::Ironwood,
                2,
                2,
            )
            .expect("NU7 supports Ironwood"),
        ),
    );
    // Distinct serialized transactions ensure the selector's txid map keeps all candidates.
    let candidates: Vec<_> = (0..83)
        .map(|i| {
            let mut tx = transaction.clone();
            tx.set_expiry_height(Height(2_000 + i));
            let tx = zebra_chain::transaction::UnminedTx::from(Arc::new(tx));
            let fee = tx.conventional_fee;
            VerifiedUnminedTx::new(tx, fee, 0, 0, Arc::new(vec![])).unwrap()
        })
        .collect();
    let extra_counts =
        ShieldedActionCounts::from_transaction(&candidates[0].transaction.transaction);
    let selected = select_mempool_transactions(
        &network,
        height,
        &miner,
        candidates,
        TransactionDependencies::default(),
        Some(&cache),
        Some(Amount::zero()),
    );
    assert_eq!(
        selected.len(),
        (330 - coinbase_counts.shielded_cost()) / 4,
        "each candidate spends four units of the budget reserved after the coinbase",
    );
    let counts = selected.iter().fold(coinbase_counts, |counts, (_, tx)| {
        counts.saturating_add(ShieldedActionCounts::from_transaction(
            &tx.transaction.transaction,
        ))
    });
    assert!(counts.exceeded_limit().is_none());
    assert!(counts
        .saturating_add(extra_counts)
        .exceeded_limit()
        .is_some());

    let fees = selected
        .iter()
        .map(|(_, tx)| tx.miner_fee)
        .sum::<zebra_chain::amount::Result<Amount<_>>>()
        .unwrap();
    let actual = TransactionTemplate::new_coinbase_with_parent_pools(
        &network,
        height,
        &miner,
        fees,
        Some(Amount::zero()),
    )
    .unwrap();
    let actual_tx: Transaction = actual.data.as_ref().zcash_deserialize_into().unwrap();
    assert_eq!(
        ShieldedActionCounts::from_transaction(&actual_tx),
        coinbase_counts
    );
    assert_eq!(
        actual.data.as_ref().len(),
        sizing_tx.zcash_serialized_size()
    );
}

/// Returns a mined transaction ID whose first four bytes in internal order are `n`, so that the IDs
/// compare in the same order as their `n`.
fn txid(n: u32) -> transaction::Hash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&n.to_be_bytes());
    transaction::Hash(bytes)
}

/// Returns the time `seconds` after a fixed time.
fn at(seconds: i64) -> Option<DateTime<Utc>> {
    Some(DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("the test time is valid"))
}

/// Returns `tx` as a mempool transaction with the mined ID `id`, that pays `fee` zatoshis, has
/// `size` bytes, and entered the mempool at `time`, with no sigops and no unpaid actions.
fn verified_tx(
    tx: Transaction,
    id: transaction::Hash,
    fee: u64,
    size: usize,
    time: Option<DateTime<Utc>>,
) -> VerifiedUnminedTx {
    let mut transaction = UnminedTx::from(Arc::new(tx));
    *transaction.id.mined_id_mut() = id;
    transaction.size = size;
    let miner_fee = Amount::try_from(fee).expect("test fees are valid amounts");

    VerifiedUnminedTx {
        fee_weight_ratio: conventional_fee_weight_ratio(&transaction, miner_fee),
        conventional_actions: conventional_actions(&transaction.transaction),
        transaction,
        miner_fee,
        legacy_sigop_count: 0,
        p2sh_sigop_count: 0,
        unpaid_actions: 0,
        time,
        height: None,
        spent_outputs: Arc::new(Vec::new()),
    }
}

/// Returns a transparent mempool transaction, see [`verified_tx()`].
fn mempool_tx(
    id: transaction::Hash,
    fee: u64,
    size: usize,
    time: Option<DateTime<Utc>>,
) -> VerifiedUnminedTx {
    verified_tx(
        Transaction::test_v1(Vec::new(), Vec::new(), LockTime::unlocked()),
        id,
        fee,
        size,
        time,
    )
}

/// Returns `tx` with `sigops` signature operations.
fn with_sigops(mut tx: VerifiedUnminedTx, sigops: u32) -> VerifiedUnminedTx {
    tx.legacy_sigop_count = sigops;
    tx
}

/// Returns `count` mempool transactions with the IDs from `first_id` on, that each pay `fee` for
/// 50,000 bytes, which is more than the tests that use them leave.
fn oversized_txs(first_id: u32, count: u32, fee: u64) -> Vec<VerifiedUnminedTx> {
    (first_id..first_id + count)
        .map(|n| mempool_tx(txid(n), fee, 50_000, at(50)))
        .collect()
}

/// Returns an NU7 transaction with `actions` Orchard actions, built from `seed`.
fn orchard_tx(actions: usize, seed: u64) -> Transaction {
    fake_v6_transaction(
        NetworkUpgrade::Nu7,
        Some(
            fake_bundle_for_branch(
                zcash_protocol::consensus::BranchId::Nu7,
                orchard::ValuePool::Orchard,
                actions,
                seed,
            )
            .expect("NU7 supports Orchard"),
        ),
        None,
    )
}

/// Records in `deps` that `child` spends an output of each of `parents`.
fn spends(
    deps: &mut TransactionDependencies,
    child: transaction::Hash,
    parents: &[transaction::Hash],
) {
    deps.add(
        child,
        parents
            .iter()
            .map(|&parent| OutPoint::from_usize(parent, 0))
            .collect(),
    );
}

/// Returns block space with `bytes` bytes left, all the sigops of a block, no unpaid actions, and
/// no shielded action limits, as before NU7.
fn block_space(bytes: usize) -> BlockSpace {
    BlockSpace {
        bytes,
        sigops: MAX_BLOCK_SIGOPS,
        unpaid_actions: BLOCK_UNPAID_ACTION_LIMIT,
        shielded: ShieldedBudget::new(&Network::Mainnet, Height(1_000_000)),
    }
}

/// Selects from `txs` into `space`, and returns the IDs of the selected transactions, in the order
/// they were selected.
fn select_ids(
    txs: Vec<VerifiedUnminedTx>,
    deps: &TransactionDependencies,
    space: BlockSpace,
) -> Vec<transaction::Hash> {
    select_with_depths(txs, deps, space)
        .into_iter()
        .map(|(id, _depth)| id)
        .collect()
}

/// Like [`select_ids()`], with the depth of each selected transaction.
fn select_with_depths(
    txs: Vec<VerifiedUnminedTx>,
    deps: &TransactionDependencies,
    space: BlockSpace,
) -> Vec<(transaction::Hash, usize)> {
    select_by_fee_rate(txs, deps, space)
        .into_iter()
        .map(|(depth, tx)| (tx.transaction.id.mined_id(), depth))
        .collect()
}

// In the tests below, a transaction of 10,000 bytes or fewer has a cost of 10,000, so a fee of
// 10,000 zatoshis is 1 zatoshi per unit of cost.
//
// Some tests leave exactly 3,000 bytes after the transactions they expect. That makes the template
// full, so another transaction that would still fit is not tried.

/// A transaction that pays more per unit of cost is tried before one that pays a higher fee.
#[test]
fn ranks_by_fee_per_cost_rather_than_by_fee() {
    let [higher_fee, higher_rate, last] = [txid(1), txid(2), txid(3)];
    let deps = TransactionDependencies::default();
    let mut txs = vec![
        // 1 zatoshi per unit of cost.
        mempool_tx(higher_fee, 20_000, 20_000, at(1)),
        // 1.5 zatoshis per unit of cost.
        mempool_tx(higher_rate, 15_000, 5_000, at(1)),
    ];

    // `higher_rate` leaves 19,000 bytes, which is too few for `higher_fee`.
    assert_eq!(
        select_ids(txs.clone(), &deps, block_space(24_000)),
        [higher_rate]
    );

    txs.push(mempool_tx(last, 800, 500, at(1)));
    assert_eq!(
        select_ids(txs, &deps, block_space(28_000)),
        [higher_rate, higher_fee]
    );
}

/// A size below 10,000 bytes doesn't change the fee per cost, and fees per cost are compared
/// exactly: equal fractions tie, so the transaction that entered the mempool first goes first, and
/// fractions too close together for floating point to tell apart don't tie.
#[test]
fn compares_fee_rates_exactly_with_a_minimum_cost() {
    let deps = TransactionDependencies::default();

    let [small, large] = [txid(1), txid(2)];
    let txs = vec![
        mempool_tx(small, 1_000, 300, at(2)),
        mempool_tx(large, 1_000, 9_999, at(1)),
    ];
    assert_eq!(
        select_ids(txs.clone(), &deps, block_space(100_000)),
        [large, small]
    );
    assert_eq!(select_ids(txs, &deps, block_space(12_999)), [large]);

    // 800 / 10,000 is the same fee per cost as 1,200 / 15,000.
    let [lower_fee, higher_fee] = [txid(3), txid(4)];
    let txs = vec![
        mempool_tx(lower_fee, 800, 2_000, at(2)),
        mempool_tx(higher_fee, 1_200, 15_000, at(1)),
    ];
    assert_eq!(
        select_ids(txs.clone(), &deps, block_space(100_000)),
        [higher_fee, lower_fee]
    );
    assert_eq!(select_ids(txs, &deps, block_space(18_000)), [higher_fee]);

    // `slightly_higher` pays 1 / 100,010,000 of a zatoshi per unit of cost more than
    // `slightly_lower`. Both fees divided by their costs round to the same `f64`, so only an exact
    // comparison puts `slightly_higher` first, although it entered the mempool later.
    let [slightly_higher, slightly_lower] = [txid(5), txid(6)];
    let txs = vec![
        mempool_tx(slightly_higher, 1_000_000_000_000_001, 10_000, at(2)),
        mempool_tx(slightly_lower, 1_000_100_000_000_001, 10_001, at(1)),
    ];
    assert_eq!(
        select_ids(txs.clone(), &deps, block_space(100_000)),
        [slightly_higher, slightly_lower]
    );
    assert_eq!(
        select_ids(txs, &deps, block_space(13_000)),
        [slightly_higher]
    );
}

/// Transactions with the same fee per cost go in the order they entered the mempool, with an
/// unknown time first, then in the order of their ID bytes in internal order.
#[test]
fn breaks_fee_rate_ties_by_mempool_time_then_by_txid() {
    let deps = TransactionDependencies::default();

    // These IDs are higher than the others, so they show that the time decides before the ID.
    let untimed = txid(0xf000_0000);
    let earliest = txid(0xf100_0000);
    // Displayed as `ff00...00`, but the lowest of these IDs in internal order.
    let mut bytes = [0; 32];
    bytes[31] = 0xff;
    let lower_id = transaction::Hash(bytes);
    // Displayed as `00...01`.
    let mut bytes = [0; 32];
    bytes[0] = 0x01;
    let higher_id = transaction::Hash(bytes);

    let txs = vec![
        mempool_tx(higher_id, 1_000, 500, at(7)),
        mempool_tx(lower_id, 1_000, 500, at(7)),
        mempool_tx(earliest, 1_000, 500, at(6)),
        mempool_tx(untimed, 1_000, 500, None),
    ];
    let expected = [untimed, earliest, lower_id, higher_id];

    // Each extra 500 bytes lets one more transaction in before 3,000 bytes are left.
    for count in 0..=expected.len() {
        assert_eq!(
            select_ids(txs.clone(), &deps, block_space(3_000 + 500 * count)),
            expected[..count].to_vec(),
            "with room for {count} transactions"
        );
    }
}

/// A transaction that pays a high fee brings in its parent that pays a low fee, ahead of an
/// unrelated transaction that pays more than the parent.
#[test]
fn ranks_a_child_together_with_its_unselected_ancestors() {
    let [parent, child, unrelated] = [txid(1), txid(2), txid(3)];
    let txs = vec![
        mempool_tx(parent, 1_000, 1_000, at(1)),
        mempool_tx(child, 30_000, 1_000, at(2)),
        mempool_tx(unrelated, 10_000, 1_000, at(3)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, child, &[parent]);

    // `child` and `parent` together pay 1.55 zatoshis per unit of cost, `unrelated` pays 1, and
    // `parent` alone pays 0.1.
    assert_eq!(
        select_with_depths(txs.clone(), &deps, block_space(5_000)),
        [(parent, 0), (child, 1)]
    );
    assert_eq!(
        select_with_depths(txs, &deps, block_space(7_000)),
        [(parent, 0), (child, 1), (unrelated, 0)]
    );
}

/// A child is ranked with the lower fee per cost of its parent, so an unrelated transaction that
/// pays less than the child alone, but more than the two together, goes first.
#[test]
fn ranks_a_child_with_its_ancestors_rather_than_alone() {
    let [parent, child, unrelated, last] = [txid(1), txid(2), txid(3), txid(4)];
    let txs = vec![
        mempool_tx(parent, 10_000, 500, at(1)),
        // 9 zatoshis per unit of cost alone, and 5 with `parent`.
        mempool_tx(child, 90_000, 500, at(2)),
        // 6 zatoshis per unit of cost.
        mempool_tx(unrelated, 60_000, 500, at(3)),
        mempool_tx(last, 800, 500, at(4)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, child, &[parent]);

    assert_eq!(
        select_ids(txs.clone(), &deps, block_space(100_000)),
        [unrelated, parent, child, last]
    );
    assert_eq!(
        select_ids(txs, &deps, block_space(4_500)),
        [unrelated, parent, child]
    );
}

/// Once the transactions a candidate depends on are in the template, it is rated on its own fee.
/// That can put it after a transaction it was ahead of, or ahead of one it was after.
#[test]
fn rates_dependents_again_once_their_dependencies_are_added() {
    // `parent` pays 6 zatoshis per unit of cost, `child` pays 0.5, or 3.25 with `parent`, and
    // `unrelated` pays 1.5.
    let [parent, child, unrelated] = [txid(1), txid(2), txid(3)];
    let txs = vec![
        mempool_tx(parent, 60_000, 1_000, at(1)),
        mempool_tx(child, 5_000, 1_000, at(2)),
        mempool_tx(unrelated, 15_000, 1_000, at(3)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, child, &[parent]);

    assert_eq!(
        select_with_depths(txs, &deps, block_space(100_000)),
        [(parent, 0), (unrelated, 0), (child, 1)]
    );

    // `first_child` pays 20 zatoshis per unit of cost, or 10.05 with `parent`, `second_child` pays
    // 3, or 1.55 with `parent`, and `unrelated` pays 2.5.
    let [parent, first_child, second_child, unrelated] = [txid(4), txid(5), txid(6), txid(7)];
    let txs = vec![
        mempool_tx(parent, 1_000, 1_000, at(1)),
        mempool_tx(first_child, 200_000, 1_000, at(2)),
        mempool_tx(second_child, 30_000, 1_000, at(3)),
        mempool_tx(unrelated, 25_000, 1_000, at(4)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, first_child, &[parent]);
    spends(&mut deps, second_child, &[parent]);

    assert_eq!(
        select_with_depths(txs, &deps, block_space(100_000)),
        [
            (parent, 0),
            (first_child, 1),
            (second_child, 1),
            (unrelated, 0)
        ]
    );
}

/// The transactions that a candidate needs go in ID order among the ones that can go next, whatever
/// their own fees and mempool times, and not one generation at a time.
#[test]
fn adds_needed_transactions_in_id_order() {
    // `low_id` pays less and entered the mempool later than `high_id`, but goes first.
    let [low_id, high_id, child] = [txid(1), txid(2), txid(3)];
    let txs = vec![
        mempool_tx(low_id, 800, 1_000, at(9)),
        mempool_tx(high_id, 5_000, 1_000, at(1)),
        mempool_tx(child, 300_000, 1_000, at(5)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, child, &[low_id, high_id]);

    assert_eq!(
        select_with_depths(txs, &deps, block_space(100_000)),
        [(low_id, 0), (high_id, 0), (child, 1)]
    );

    // `grandchild` spends from `middle` and `second_dependency`, and `middle` spends from
    // `first_dependency`. Their IDs are in that order: `first_dependency`, `middle`,
    // `second_dependency`, `grandchild`.
    let [first_dependency, middle, second_dependency, grandchild] =
        [txid(4), txid(5), txid(6), txid(7)];
    let txs = vec![
        mempool_tx(first_dependency, 1_000, 1_000, at(1)),
        mempool_tx(middle, 1_000, 1_000, at(1)),
        mempool_tx(second_dependency, 1_000, 1_000, at(1)),
        mempool_tx(grandchild, 40_000, 1_000, at(1)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, middle, &[first_dependency]);
    spends(&mut deps, grandchild, &[middle, second_dependency]);

    assert_eq!(
        select_with_depths(txs, &deps, block_space(100_000)),
        [
            (first_dependency, 0),
            (middle, 1),
            (second_dependency, 0),
            (grandchild, 2)
        ]
    );
}

/// A candidate that doesn't fit doesn't stop selection, and the transactions it depends on can
/// still go in with another candidate.
#[test]
fn keeps_trying_after_a_transaction_does_not_fit() {
    let [too_big, small, last] = [txid(1), txid(2), txid(3)];
    let txs = vec![
        mempool_tx(too_big, 100_000, 50_000, at(1)),
        mempool_tx(small, 10_000, 1_000, at(2)),
        mempool_tx(last, 800, 500, at(3)),
    ];
    let deps = TransactionDependencies::default();
    assert_eq!(
        select_ids(txs.clone(), &deps, block_space(10_000)),
        [small, last]
    );
    assert_eq!(select_ids(txs, &deps, block_space(4_000)), [small]);

    // `big_dependent` and `small_dependent` both spend from `dependency`. With it,
    // `big_dependent` pays 8.35 zatoshis per unit of cost but doesn't fit, and then
    // `small_dependent` pays 2.55 and brings `dependency` in.
    let [dependency, big_dependent, small_dependent] = [txid(4), txid(5), txid(6)];
    let txs = vec![
        mempool_tx(dependency, 1_000, 1_000, at(1)),
        mempool_tx(big_dependent, 500_000, 50_000, at(2)),
        mempool_tx(small_dependent, 50_000, 1_000, at(3)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, big_dependent, &[dependency]);
    spends(&mut deps, small_dependent, &[dependency]);

    assert_eq!(
        select_with_depths(txs, &deps, block_space(20_000)),
        [(dependency, 0), (small_dependent, 1)]
    );
}

/// A candidate that would go over a [ZIP 218] shielded action limit uses none of the shielded
/// budget, so the budget is still there for the next candidates, including the transactions that
/// the candidate depends on.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
#[test]
fn a_failed_try_uses_none_of_the_shielded_budget() {
    let [parent, child, unrelated, transparent] = [txid(1), txid(2), txid(3), txid(4)];
    let txs = vec![
        verified_tx(orchard_tx(2, 1), parent, 2_000, 1_000, at(1)),
        verified_tx(orchard_tx(3, 10), child, 200_000, 1_000, at(2)),
        verified_tx(orchard_tx(2, 20), unrelated, 50_000, 1_000, at(3)),
        mempool_tx(transparent, 1_000, 1_000, at(4)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, child, &[parent]);

    // 4 Orchard actions are left under the block limits.
    let space = BlockSpace {
        shielded: ShieldedBudget {
            applies: true,
            used: ShieldedActionCounts {
                orchard_actions: 326,
                ..ShieldedActionCounts::default()
            },
        },
        ..block_space(100_000)
    };

    // `child` with `parent` needs 5 Orchard actions and doesn't fit. Then `unrelated` and `parent`
    // use up the 4 actions, so `child` doesn't fit alone either, and `transparent` still does.
    assert_eq!(
        select_ids(txs, &deps, space),
        [unrelated, parent, transparent]
    );
}

/// A candidate that is tried again after a transaction it depends on goes in, and still doesn't
/// fit, counts as another failed try.
#[test]
fn counts_a_retried_candidate_as_another_failed_try() {
    let [parent, child, small] = [txid(1), txid(2), txid(3)];
    let mut txs = vec![
        mempool_tx(parent, 4_000, 8_000, at(1)),
        mempool_tx(small, 1_000, 1_000, at(3)),
    ];
    txs.extend(oversized_txs(1_000, 999, 15_000));

    // `parent` goes in first, and then 999 tries fail before `small` is tried.
    assert_eq!(
        select_ids(
            txs.clone(),
            &TransactionDependencies::default(),
            block_space(20_000)
        ),
        [parent, small]
    );

    // `child` with `parent` needs 21,000 bytes and doesn't fit. After `parent` goes in, `child`
    // alone needs 13,000 of the 12,000 bytes left, so with the 999 after it, 1,000 tries in a row
    // fail before `small`.
    txs.push(mempool_tx(child, 130_000, 13_000, at(2)));
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, child, &[parent]);

    assert_eq!(select_ids(txs, &deps, block_space(20_000)), [parent]);
}

/// A template with 3,000 unused bytes or fewer is full, even if a transaction would still fit.
#[test]
fn treats_the_template_as_full_with_three_thousand_bytes_or_fewer_left() {
    let deps = TransactionDependencies::default();

    // 400 bytes each, tried in the order they entered the mempool.
    let ids: Vec<_> = (1..=10).map(txid).collect();
    let txs: Vec<_> = ids
        .iter()
        .zip(1..)
        .map(|(id, seconds)| mempool_tx(*id, 1_000, 400, at(seconds)))
        .collect();

    for (bytes, count) in [
        (2_000, 0),
        (3_000, 0),
        (3_001, 1),
        (3_400, 1),
        (3_401, 2),
        (3_800, 2),
        (3_801, 3),
    ] {
        assert_eq!(
            select_ids(txs.clone(), &deps, block_space(bytes)),
            ids[..count].to_vec(),
            "with {bytes} bytes left"
        );
    }
}

/// A template is full after 1,000 tries in a row fail. A transaction that goes in starts the count
/// again. Transactions that depend on one that is not in the mempool, or that spend from each other
/// in a cycle, are never tried, so they don't count.
#[test]
fn treats_the_template_as_full_after_a_thousand_failed_tries_in_a_row() {
    let [first_in_cycle, second_in_cycle, orphan, orphan_child, middle, last] =
        [txid(1), txid(2), txid(3), txid(4), txid(5), txid(6)];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, first_in_cycle, &[second_in_cycle]);
    spends(&mut deps, second_in_cycle, &[first_in_cycle]);
    spends(&mut deps, orphan, &[txid(0xdead)]);
    spends(&mut deps, orphan_child, &[orphan]);

    for (tries_after_middle, expected) in [(999, vec![middle, last]), (1_000, vec![middle])] {
        // 999 tries fail before `middle`, and `tries_after_middle` after it.
        let mut txs = vec![
            mempool_tx(first_in_cycle, 1_000_000, 500, at(0)),
            mempool_tx(second_in_cycle, 1_000_000, 500, at(0)),
            mempool_tx(orphan, 1_000_000, 500, at(0)),
            mempool_tx(orphan_child, 1_000_000, 500, at(0)),
            mempool_tx(middle, 4_000, 1_000, at(1)),
            mempool_tx(last, 1_000, 1_000, at(2)),
        ];
        txs.extend(oversized_txs(1_000, 999, 25_000));
        txs.extend(oversized_txs(10_000, tries_after_middle, 15_000));

        assert_eq!(
            select_ids(txs, &deps, block_space(40_000)),
            expected,
            "with {tries_after_middle} failed tries after `middle`"
        );
    }
}

/// A transaction with unpaid actions doesn't fit, because blocks allow none, and that counts as a
/// failed try.
#[test]
fn counts_unpaid_actions_as_a_failed_try() {
    let [unpaid, paid] = [txid(1), txid(2)];
    let deps = TransactionDependencies::default();

    for (other_failed_tries, expected) in [(0, vec![paid]), (998, vec![paid]), (999, vec![])] {
        let mut unpaid_tx = mempool_tx(unpaid, 50_000, 1_000, at(1));
        unpaid_tx.unpaid_actions = 1;
        let mut txs = vec![unpaid_tx, mempool_tx(paid, 1_000, 1_000, at(2))];
        txs.extend(oversized_txs(1_000, other_failed_tries, 15_000));

        assert_eq!(
            select_ids(txs, &deps, block_space(40_000)),
            expected,
            "with {other_failed_tries} other failed tries"
        );
    }
}

/// Transactions with an ancestor that is neither selected nor in the mempool, or that spend from
/// each other in a cycle, are never selected, and don't keep other transactions out.
#[test]
fn never_selects_transactions_without_an_order_after_their_ancestors() {
    let [orphan, orphan_child, first_in_cycle, second_in_cycle, unrelated] =
        [txid(1), txid(2), txid(3), txid(4), txid(5)];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, orphan, &[txid(0xdead)]);
    spends(&mut deps, orphan_child, &[orphan]);
    spends(&mut deps, first_in_cycle, &[second_in_cycle]);
    spends(&mut deps, second_in_cycle, &[first_in_cycle]);

    let never_selected = vec![
        mempool_tx(orphan, 1_000_000, 500, at(1)),
        mempool_tx(orphan_child, 1_000_000, 500, at(2)),
        mempool_tx(first_in_cycle, 1_000_000, 500, at(3)),
        mempool_tx(second_in_cycle, 1_000_000, 500, at(4)),
    ];
    let mut txs = never_selected.clone();
    txs.push(mempool_tx(unrelated, 1_000, 500, at(5)));

    assert_eq!(select_ids(txs, &deps, block_space(100_000)), [unrelated]);
    assert_eq!(
        select_ids(never_selected, &deps, block_space(100_000)),
        vec![]
    );
    assert_eq!(select_ids(Vec::new(), &deps, block_space(100_000)), vec![]);
}

/// The same mempool gives the same selection, whatever order the mempool lists it in.
#[test]
fn selection_does_not_depend_on_mempool_order() {
    let network = Network::Mainnet;
    let miner_params =
        MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20])));

    let [parent, first_child, second_child, unrelated, untimed, earliest] = [
        txid(1),
        txid(2),
        txid(3),
        txid(4),
        txid(0xf000_0000),
        txid(0xf100_0000),
    ];
    let mut bytes = [0; 32];
    bytes[31] = 0xff;
    let lower_id = transaction::Hash(bytes);
    let mut bytes = [0; 32];
    bytes[0] = 0x01;
    let higher_id = transaction::Hash(bytes);

    let txs = vec![
        mempool_tx(parent, 1_000, 500, at(1)),
        mempool_tx(first_child, 200_000, 500, at(2)),
        mempool_tx(second_child, 30_000, 500, at(3)),
        mempool_tx(unrelated, 25_000, 500, at(4)),
        mempool_tx(untimed, 1_000, 500, None),
        mempool_tx(earliest, 1_000, 500, at(6)),
        mempool_tx(lower_id, 1_000, 500, at(7)),
        mempool_tx(higher_id, 1_000, 500, at(7)),
    ];
    let mut deps = TransactionDependencies::default();
    spends(&mut deps, first_child, &[parent]);
    spends(&mut deps, second_child, &[parent]);

    // `first_child` brings `parent` in, then `second_child` pays more alone than `unrelated`, and
    // the rest pay the same, so their mempool times and IDs decide.
    let expected = [
        (parent, 0),
        (first_child, 1),
        (second_child, 1),
        (unrelated, 0),
        (untimed, 0),
        (earliest, 0),
        (lower_id, 0),
        (higher_id, 0),
    ];

    let mut reversed = txs.clone();
    reversed.reverse();
    let mut rotated = txs.clone();
    rotated.rotate_left(3);

    for mempool_txs in [txs, reversed, rotated] {
        let selected: Vec<_> = select_mempool_transactions(
            &network,
            Height(1_000_000),
            &miner_params,
            mempool_txs,
            deps.clone(),
            None,
            None,
        )
        .into_iter()
        .map(|(depth, tx)| (tx.transaction.id.mined_id(), depth))
        .collect();

        assert_eq!(selected, expected);
    }
}

/// A template is full after 1,000 tries in a row fail, even when it is nearly empty.
#[test]
fn treats_a_nearly_empty_template_as_full_after_a_thousand_failed_tries() {
    let network = Network::Mainnet;
    let miner_params =
        MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20])));
    let cheap = txid(1);

    for (count, expected) in [(999, vec![cheap]), (1_000, vec![])] {
        // Each of these has more sigops than a whole block allows.
        let mut txs: Vec<_> = (100..100 + count)
            .map(|n| {
                with_sigops(
                    mempool_tx(txid(n), 1_000_000, 500, at(1)),
                    MAX_BLOCK_SIGOPS + 1,
                )
            })
            .collect();
        txs.push(mempool_tx(cheap, 800, 500, at(2)));

        let selected: Vec<_> = select_mempool_transactions(
            &network,
            Height(1_000_000),
            &miner_params,
            txs,
            TransactionDependencies::default(),
            None,
            None,
        )
        .into_iter()
        .map(|(_depth, tx)| tx.transaction.id.mined_id())
        .collect();

        assert_eq!(
            selected, expected,
            "with {count} transactions that never fit"
        );
    }
}
