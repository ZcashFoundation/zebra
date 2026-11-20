//! Tests for ZIP-317 transaction selection for block template production

#![allow(clippy::unwrap_in_result)]

use std::{collections::HashSet, sync::Arc};

use rand::{rngs::StdRng, SeedableRng};

use zcash_keys::address::Address;
use zcash_transparent::address::TransparentAddress;

use zebra_chain::{
    amount::Amount,
    block::{Header, Height, MAX_BLOCK_BYTES},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashSerialize,
    transaction::{self, LockTime, Transaction, VerifiedUnminedTx},
    transparent::{Input, OutPoint, Output, Script},
};
use zebra_node_services::mempool::TransactionDependencies;

use crate::methods::types::{get_block_template::MinerParams, transaction::TransactionTemplate};

use super::{fee_weighted_order, max_transaction_count_size, select_mempool_transactions};

/// Distinct, transparent-only candidates small enough that 18,400 fit in one block.
fn transparent_candidates(count: usize) -> Vec<VerifiedUnminedTx> {
    (0..count)
        .map(|index| {
            let tx = Transaction::test_v5(
                NetworkUpgrade::Nu5,
                vec![Input::PrevOut {
                    outpoint: OutPoint::from_usize(transaction::Hash([1; 32]), index),
                    unlock_script: Script::new(&[]),
                    sequence: u32::MAX,
                }],
                vec![Output {
                    value: Amount::new(1_000),
                    lock_script: Script::new(&[0x51]),
                }],
                LockTime::unlocked(),
                Height(2_000_000),
            );
            VerifiedUnminedTx::new(
                Arc::new(tx).into(),
                Amount::new(10_000 * (1 + i64::try_from(index % 4).unwrap())),
                0,
                0,
                Default::default(),
                &Network::Mainnet,
                Height(2_000_000),
            )
            .unwrap()
        })
        .collect()
}

/// Candidates are ordered by their fee weight ratio, highest first, and by transaction ID
/// within a ratio. The order of the candidate list doesn't change the result.
#[test]
fn candidates_are_ordered_by_fee_weight_ratio_then_transaction_id() {
    use rand::seq::SliceRandom;

    // Four fee levels, three candidates at each.
    let mut candidates = transparent_candidates(12);
    let ordered: Vec<_> = fee_weighted_order(candidates.clone()).collect();

    assert_eq!(ordered.len(), candidates.len());
    for pair in ordered.windows(2) {
        let [first, second] = pair else {
            unreachable!("windows of two candidates");
        };
        assert!(
            first.fee_weight_ratio > second.fee_weight_ratio
                || (first.fee_weight_ratio == second.fee_weight_ratio
                    && first.transaction.id.mined_id() < second.transaction.id.mined_id()),
            "a candidate comes before any with a lower ratio, or with the same ratio and a \
             higher transaction ID",
        );
    }
    assert!(
        ordered[0].fee_weight_ratio > ordered[11].fee_weight_ratio,
        "the candidates have different ratios",
    );

    let ids = |transactions: &[VerifiedUnminedTx]| -> Vec<_> {
        transactions.iter().map(|tx| tx.transaction.id).collect()
    };
    for seed in 0..20 {
        candidates.shuffle(&mut StdRng::seed_from_u64(seed));
        let reordered: Vec<_> = fee_weighted_order(candidates.clone()).collect();
        assert_eq!(ids(&reordered), ids(&ordered));
    }
}

/// Transactions that spend each other are taken in a fixed order too: each level of dependents
/// by fee weight ratio and then transaction ID, whatever order their dependencies were added in.
#[test]
fn dependents_are_selected_in_a_fixed_order() {
    use rand::seq::SliceRandom;

    let candidates = transparent_candidates(13);
    let id = |index: usize| candidates[index].transaction.id.mined_id();

    // Candidate 0 spends no other candidate, 1 to 6 spend it, and 7 to 12 spend candidate 1.
    let mut links: Vec<(usize, usize)> = (1..=6).map(|child| (child, 0)).collect();
    links.extend((7..=12).map(|grandchild| (grandchild, 1)));

    let in_order = |mut level: Vec<usize>| {
        level.sort_by(|&left, &right| {
            candidates[right]
                .fee_weight_ratio
                .total_cmp(&candidates[left].fee_weight_ratio)
                .then_with(|| id(left).cmp(&id(right)))
        });
        level
    };
    let expected: Vec<_> = [
        vec![0],
        in_order((1..=6).collect()),
        in_order((7..=12).collect()),
    ]
    .concat()
    .into_iter()
    .map(id)
    .collect();

    for seed in 0..10 {
        links.shuffle(&mut StdRng::seed_from_u64(seed));
        let mut mempool_tx_deps = TransactionDependencies::default();
        for &(dependent, dependency) in &links {
            mempool_tx_deps.add(
                id(dependent),
                vec![OutPoint::from_usize(id(dependency), dependent)],
            );
        }

        let selected: Vec<_> = select_mempool_transactions(
            &Network::Mainnet,
            Height(2_000_000),
            &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
            candidates.clone(),
            mempool_tx_deps,
            None,
        )
        .into_iter()
        .map(|(_depth, tx)| tx.transaction.id.mined_id())
        .collect();

        assert_eq!(selected, expected);
    }
}

#[test]
fn selects_all_large_mempool_candidates_when_they_fit() {
    let candidates = transparent_candidates(18_400);
    let expected: HashSet<_> = candidates.iter().map(|tx| tx.transaction.id).collect();
    let selected = select_mempool_transactions(
        &Network::Mainnet,
        Height(2_000_000),
        &MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20]))),
        candidates,
        TransactionDependencies::default(),
        None,
    );
    assert_eq!(
        selected.len(),
        expected.len(),
        "no candidates are duplicated"
    );
    assert_eq!(
        selected
            .into_iter()
            .map(|(_, tx)| tx.transaction.id)
            .collect::<HashSet<_>>(),
        expected,
        "every fitting candidate is selected"
    );
}

#[test]
fn skips_zero_weights_and_rejects_invalid_weights() {
    let cases: &[(&[f32], usize)] = &[
        (&[], 0),
        (&[0.0, 0.0], 0),
        (&[0.0, 1.0, 0.0], 1),
        (&[-1.0, 1.0], 0),
        (&[f32::NAN, 1.0], 0),
    ];
    for &(weights, expected_count) in cases {
        let mut candidates = transparent_candidates(weights.len());
        for (tx, &weight) in candidates.iter_mut().zip(weights) {
            tx.fee_weight_ratio = weight;
        }
        // Template selection recomputes weights from fees, so check the order directly.
        let selected: Vec<_> = fee_weighted_order(candidates).collect();
        assert_eq!(selected.len(), expected_count, "weights: {weights:?}");
        assert!(selected.iter().all(|tx| tx.fee_weight_ratio > 0.0));
    }
}

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

    let template = transparent_candidates(1).pop().unwrap();
    let transaction_with_script = |script_size| {
        Arc::new(
            template
                .transaction
                .transaction
                .as_ref()
                .clone()
                .with_transparent_inputs(vec![Input::PrevOut {
                    outpoint: OutPoint::from_usize(transaction::Hash([1; 32]), 0),
                    unlock_script: Script::new(&vec![0; script_size]),
                    sequence: u32::MAX,
                }]),
        )
    };
    // Both boundary cases use a five-byte CompactSize for the script length.
    let transaction_overhead = transaction_with_script(65_536).zcash_serialized_size() - 65_536;
    let transaction_with_size = |size| {
        let transaction: transaction::UnminedTx =
            transaction_with_script(size - transaction_overhead).into();
        let miner_fee = transaction::zip317::conventional_fee(
            transaction.conventional_actions,
            &network,
            height,
        );
        VerifiedUnminedTx::new(
            transaction,
            miner_fee,
            0,
            0,
            Default::default(),
            &network,
            height,
        )
        .unwrap()
    };
    let unmined_tx = transaction_with_size(safe_budget);

    assert_eq!(
        select_mempool_transactions(
            &network,
            height,
            &miner_params,
            vec![unmined_tx.clone()],
            TransactionDependencies::default(),
            None,
        )
        .len(),
        1,
        "should select a transaction exactly filling the safe block budget"
    );

    let unmined_tx = transaction_with_size(safe_budget + 1);

    assert_eq!(
        select_mempool_transactions(
            &network,
            height,
            &miner_params,
            vec![unmined_tx],
            TransactionDependencies::default(),
            None,
        ),
        vec![],
        "should not select a transaction one byte over the safe block budget"
    );
}

/// Orchard and Ironwood candidates must share the ZIP-218 budget with the Sapling coinbase.
#[test]
fn reserves_shielded_budget_for_sapling_coinbase() {
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
    let sizing = TransactionTemplate::coinbase_resource_usage(
        &network,
        height,
        &miner,
        Some(Amount::zero()),
    )
    .unwrap();
    let coinbase_counts = sizing.shielded_actions;
    assert!(coinbase_counts.sapling_io > 0);

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
            let fee =
                transaction::zip317::conventional_fee(tx.conventional_actions, &network, height);
            VerifiedUnminedTx::new(tx, fee, 0, 0, Arc::new(vec![]), &network, height).unwrap()
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
    assert_eq!(actual_tx.zcash_serialized_size(), sizing.serialized_size);
}

#[test]
fn fee_activation_refreshes_template_candidates() {
    use std::sync::Arc;
    use zebra_chain::transaction::{LockTime, Transaction, UnminedTx, VerifiedUnminedTx};

    let transaction = UnminedTx::from(Arc::new(Transaction::test_v1(
        vec![],
        vec![],
        LockTime::unlocked(),
    )));
    let miner = MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20])));

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        let activation = network.fee_reduction_height();
        let before = activation.previous().unwrap();
        let low_fee = VerifiedUnminedTx::new(
            transaction.clone(),
            2_000.try_into().unwrap(),
            0,
            0,
            Arc::new(vec![]),
            &network,
            activation,
        )
        .unwrap();

        // An entry accepted on a newer tip must not be selected before activation after a reorg.
        for (height, expected_count) in [(before, 0), (activation, 1)] {
            let selected = select_mempool_transactions(
                &network,
                height,
                &miner,
                vec![low_fee.clone()],
                TransactionDependencies::default(),
                Some(Amount::zero()),
            );
            assert_eq!(selected.len(), expected_count);
        }

        let high_fee = VerifiedUnminedTx::new(
            transaction.clone(),
            100_000.try_into().unwrap(),
            0,
            0,
            Arc::new(vec![]),
            &network,
            before,
        )
        .unwrap();
        assert_eq!(high_fee.fee_weight_ratio, 4.0);
        let selected = select_mempool_transactions(
            &network,
            activation,
            &miner,
            vec![high_fee],
            TransactionDependencies::default(),
            Some(Amount::zero()),
        );
        assert_eq!(selected.len(), 1);
        // The fee is 125 times the reduced conventional fee, and the reduced policy has no cap.
        assert_eq!(selected[0].1.fee_weight_ratio, 125.0);
    }
}
