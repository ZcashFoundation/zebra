//! Regression tests for verified-set indexes and incremental metrics.

use std::sync::Arc;

use rand::{
    distr::{Distribution, Uniform},
    rngs::StdRng,
    SeedableRng,
};
use zebra_chain::parameters::Network;

use super::*;

fn template() -> VerifiedUnminedTx {
    Network::Mainnet
        .unmined_transactions_in_blocks(..=10)
        .next()
        .expect("the first mainnet block fixture has a coinbase transaction")
}

fn transaction(
    template: &VerifiedUnminedTx,
    nonce: u32,
    parent: Option<transaction::Hash>,
) -> VerifiedUnminedTx {
    let outpoint = parent.map_or(
        transparent::OutPoint {
            hash: transaction::Hash([0xff; 32]),
            index: nonce,
        },
        |hash| transparent::OutPoint { hash, index: 0 },
    );
    let tx = template
        .transaction
        .transaction
        .as_ref()
        .clone()
        .with_transparent_inputs(vec![transparent::Input::PrevOut {
            outpoint,
            unlock_script: transparent::Script::new(&[]),
            sequence: nonce,
        }]);
    let mut verified = template.clone();
    verified.transaction = UnminedTx::from(Arc::new(tx));
    verified
}

/// The old full-pool aggregation, independent of the incremental update helper.
fn assert_metrics_match_rescan(set: &VerifiedSet) {
    let mut expected = MempoolMetrics::default();
    let mut size = 0;
    let mut cost = 0;
    for entry in set.transactions().values() {
        size += entry.transaction.size;
        cost += entry.cost();
        expected.paid_actions += entry.conventional_actions - entry.unpaid_actions;
        if entry.fee_weight_ratio > 3.0 {
            expected.size_by_weight[4] += entry.transaction.size;
        } else if entry.fee_weight_ratio > 2.0 {
            expected.size_by_weight[3] += entry.transaction.size;
        } else if entry.fee_weight_ratio > 1.0 {
            expected.size_by_weight[2] += entry.transaction.size;
        } else if entry.fee_weight_ratio == 1.0 {
            expected.size_by_weight[1] += entry.transaction.size;
        } else {
            expected.size_by_weight[0] += entry.transaction.size;
            if entry.fee_weight_ratio < 0.2 {
                expected.unpaid_actions[0] += entry.unpaid_actions;
            } else if entry.fee_weight_ratio < 0.4 {
                expected.unpaid_actions[1] += entry.unpaid_actions;
            } else if entry.fee_weight_ratio < 0.6 {
                expected.unpaid_actions[2] += entry.unpaid_actions;
            } else if entry.fee_weight_ratio < 0.8 {
                expected.unpaid_actions[3] += entry.unpaid_actions;
            } else {
                expected.unpaid_actions[4] += entry.unpaid_actions;
            }
        }
    }
    assert_eq!(set.metrics.paid_actions, expected.paid_actions);
    assert_eq!(set.metrics.unpaid_actions, expected.unpaid_actions);
    assert_eq!(set.metrics.size_by_weight, expected.size_by_weight);
    assert_eq!(set.total_serialized_size(), size);
    assert_eq!(set.total_cost(), cost);
}

#[test]
fn verified_set_metrics_bucket_boundaries() {
    let template = template();
    let mut pending = PendingOutputs::default();
    // Expected (unpaid bucket, size bucket) just below, at, and just above each boundary.
    let boundaries = [
        (0.2_f32, [(Some(0), 0), (Some(1), 0), (Some(1), 0)]),
        (0.4_f32, [(Some(1), 0), (Some(2), 0), (Some(2), 0)]),
        (0.6_f32, [(Some(2), 0), (Some(3), 0), (Some(3), 0)]),
        (0.8_f32, [(Some(3), 0), (Some(4), 0), (Some(4), 0)]),
        (1.0_f32, [(Some(4), 0), (None, 1), (None, 2)]),
        (2.0_f32, [(None, 2), (None, 2), (None, 3)]),
        (3.0_f32, [(None, 3), (None, 3), (None, 4)]),
    ];
    for (boundary, buckets) in boundaries {
        let bits = boundary.to_bits();
        for (bits, (unpaid_bucket, size_bucket)) in
            [bits - 1, bits, bits + 1].into_iter().zip(buckets)
        {
            let mut set = VerifiedSet::default();
            let mut tx = transaction(&template, 0, None);
            // Synthetic cached values isolate bucket accounting from fee verification.
            tx.fee_weight_ratio = f32::from_bits(bits);
            tx.conventional_actions = 9;
            tx.unpaid_actions = if unpaid_bucket.is_some() { 4 } else { 0 };
            let size = tx.transaction.size;
            let id = tx.transaction.id.mined_id();
            set.insert(tx, Vec::new(), &mut pending, None)
                .expect("a fresh set has no spend conflicts");

            let mut unpaid = [0; 5];
            if let Some(bucket) = unpaid_bucket {
                unpaid[bucket] = 4;
            }
            let mut sizes = [0; 5];
            sizes[size_bucket] = size;
            assert_eq!(set.metrics.unpaid_actions, unpaid);
            assert_eq!(set.metrics.size_by_weight, sizes);
            assert_metrics_match_rescan(&set);
            assert_eq!(set.remove(&id).len(), 1);
            assert_metrics_match_rescan(&set);
        }
    }
}

#[test]
fn verified_set_metrics_randomized_mutations() {
    let template = template();
    let mut pending = PendingOutputs::default();
    let mut set = VerifiedSet::default();
    let mut rng = StdRng::seed_from_u64(268);
    let operation = Uniform::new(0, 16).expect("the operation range is nonempty");
    let weights = [0.0, 0.1, 0.2, 0.4, 0.6, 0.8, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5];
    let weight_index = Uniform::new(0, weights.len()).expect("the weight range is nonempty");
    let sizes = Uniform::new(100, 30_000).expect("the size range is nonempty");
    let mut nonce = 0;
    let mut operation_counts = [0; 5];

    for _ in 0..512 {
        let mut ids: Vec<_> = set.transactions().keys().copied().collect();
        ids.sort_unstable_by_key(|id| id.0);
        let id = if ids.is_empty() {
            None
        } else {
            let index = Uniform::new(0, ids.len()).expect("the set is nonempty");
            Some(ids[index.sample(&mut rng)])
        };
        match operation.sample(&mut rng) {
            0..=7 => {
                let parent = id.filter(|&hash| {
                    !set.has_spent_outpoint(&transparent::OutPoint { hash, index: 0 })
                });
                let mut tx = transaction(&template, nonce, parent);
                nonce += 1;
                tx.fee_weight_ratio = weights[weight_index.sample(&mut rng)];
                tx.conventional_actions = 9;
                tx.unpaid_actions = if tx.fee_weight_ratio < 1.0 { 4 } else { 0 };
                tx.transaction.size = sizes.sample(&mut rng);
                let spent = parent
                    .map(|hash| transparent::OutPoint { hash, index: 0 })
                    .into_iter()
                    .collect();
                set.insert(tx, spent, &mut pending, None).expect(
                    "the transaction spends an available output or a unique external input",
                );
                operation_counts[0] += 1;
            }
            8..=10 => {
                if let Some(id) = id {
                    assert!(!set.remove(&id).is_empty());
                    operation_counts[1] += 1;
                }
            }
            11..=12 => {
                set.remove_all(ids.into_iter().filter(|id| id.0[0] % 2 == 0));
                operation_counts[2] += 1;
            }
            13..=14 => {
                // Leave a singleton so the production random eviction has a deterministic victim.
                for id in ids {
                    if set.transaction_count() <= 1 {
                        break;
                    }
                    if set.contains(&id) {
                        set.remove(&id);
                        assert_metrics_match_rescan(&set);
                    }
                }
                if set.transaction_count() == 0 {
                    let tx = transaction(&template, nonce, None);
                    nonce += 1;
                    set.insert(tx, Vec::new(), &mut pending, None)
                        .expect("a fresh set has no spend conflicts");
                    assert_metrics_match_rescan(&set);
                }
                assert_eq!(set.evict_one().len(), 1);
                assert_eq!(set.transaction_count(), 0);
                operation_counts[3] += 1;
            }
            _ => {
                set.clear();
                operation_counts[4] += 1;
            }
        }
        assert_metrics_match_rescan(&set);
    }
    assert!(operation_counts.into_iter().all(|count| count > 0));
    set.clear();
    assert_metrics_match_rescan(&set);
}

#[test]
fn verified_set_metrics_dependent_removals() {
    let template = template();
    let mut pending = PendingOutputs::default();
    let mut set = VerifiedSet::default();
    let mut chain = Vec::new();
    let mut parent = None;
    for (nonce, weight) in [0.1, 0.5, 1.0, 3.5].into_iter().enumerate() {
        let mut tx = transaction(
            &template,
            nonce.try_into().expect("four nonces fit in u32"),
            parent,
        );
        tx.fee_weight_ratio = weight;
        tx.conventional_actions = 9;
        tx.unpaid_actions = if weight < 1.0 { 4 } else { 0 };
        let id = tx.transaction.id.mined_id();
        let spent = parent
            .map(|hash| transparent::OutPoint { hash, index: 0 })
            .into_iter()
            .collect::<Vec<_>>();
        set.insert(tx.clone(), spent.clone(), &mut pending, None)
            .expect("each transaction spends its parent's unspent output");
        chain.push((tx, spent));
        parent = Some(id);
        assert_metrics_match_rescan(&set);
    }
    let root = chain[0].0.transaction.id.mined_id();
    let conflict = transaction(&template, 10, Some(root));
    assert_eq!(
        set.insert(conflict, chain[1].1.clone(), &mut pending, None),
        Err(SameEffectsTipRejectionError::SpendConflict)
    );
    assert_metrics_match_rescan(&set);

    // Matching both an ancestor and its descendants must subtract each contribution only once.
    assert_eq!(
        set.remove_all(chain.iter().map(|(tx, _)| tx.transaction.id.mined_id()))
            .len(),
        chain.len()
    );
    assert_metrics_match_rescan(&set);
    assert_eq!(
        set.insert(chain[1].0.clone(), chain[1].1.clone(), &mut pending, None),
        Err(SameEffectsTipRejectionError::MissingOutput)
    );
    assert_metrics_match_rescan(&set);

    for (tx, spent) in &chain {
        set.insert(tx.clone(), spent.clone(), &mut pending, None)
            .expect("the chain is reinserted in dependency order");
        assert_metrics_match_rescan(&set);
    }
    // A mined parent no longer invalidates descendants when it is removed.
    set.clear_mined_dependencies(&HashSet::from([root]));
    assert_metrics_match_rescan(&set);
    assert_eq!(set.remove(&root).len(), 1);
    assert_metrics_match_rescan(&set);
    let child = chain[1].0.transaction.id.mined_id();
    assert_eq!(set.remove(&child).len(), chain.len() - 1);
    assert_metrics_match_rescan(&set);
    set.clear();
    set.clear();
    assert_metrics_match_rescan(&set);
}

#[test]
fn verified_set_mined_v5_removes_all_output_indexes() {
    let template = template();
    let output = template.transaction.transaction.outputs()[0].clone();
    let [mined, survivor] = [0, 1].map(|nonce| {
        let mut verified = transaction(&template, nonce, None);
        verified.transaction = Arc::new(transaction::Transaction::test_v5(
            zebra_chain::parameters::NetworkUpgrade::Nu5,
            verified.transaction.transaction.inputs(),
            vec![output.clone(); 3],
            transaction::LockTime::unlocked(),
            Height(2_000_000),
        ))
        .into();
        verified
    });
    let mut pending = PendingOutputs::default();
    let mut set = VerifiedSet::default();
    for tx in [&mined, &survivor] {
        set.insert(tx.clone(), Vec::new(), &mut pending, None)
            .expect("the transactions spend distinct external outputs");
        for index in 0..3 {
            let outpoint = transparent::OutPoint::from_usize(tx.transaction.id.mined_id(), index);
            assert_eq!(set.created_output(&outpoint), Some(output.clone()));
        }
        for outpoint in tx.transaction.transaction.spent_outpoints() {
            assert!(set.has_spent_outpoint(&outpoint));
        }
    }

    let mined_ids = HashSet::from([mined.transaction.id.mined_id()]);
    set.clear_mined_dependencies(&mined_ids);
    assert_eq!(
        set.remove_all(mined_ids),
        HashSet::from([mined.transaction.id])
    );
    assert_eq!(set.transaction_count(), 1);
    for (tx, present) in [(&mined, false), (&survivor, true)] {
        for index in 0..3 {
            let outpoint = transparent::OutPoint::from_usize(tx.transaction.id.mined_id(), index);
            assert_eq!(
                set.created_output(&outpoint),
                present.then(|| output.clone())
            );
        }
        for outpoint in tx.transaction.transaction.spent_outpoints() {
            assert_eq!(set.has_spent_outpoint(&outpoint), present);
        }
    }
}

#[test]
fn known_ids_ignore_unknown_duplicates_and_removed_descendants() {
    let template = template();
    let parent = transaction(&template, 0, None);
    let parent_id = parent.transaction.id;
    let child = transaction(&template, 1, Some(parent_id.mined_id()));
    let child_id = child.transaction.id;
    let survivor = transaction(&template, 2, None);
    let survivor_id = survivor.transaction.id;
    let survivor_size = survivor.transaction.size;
    let survivor_cost = survivor.cost();
    let output = transparent::OutPoint::from_usize(parent_id.mined_id(), 0);
    let mut pending = PendingOutputs::default();
    let mut set = VerifiedSet::default();
    for (tx, spent) in [(parent, vec![]), (child, vec![output]), (survivor, vec![])] {
        set.insert(tx, spent, &mut pending, None)
            .expect("the transactions spend distinct available outputs");
    }
    let missing = transaction::Hash([0xfe; 32]);
    assert_eq!(
        set.remove_all([
            missing,
            parent_id.mined_id(),
            parent_id.mined_id(),
            child_id.mined_id(),
            missing,
        ]),
        HashSet::from([parent_id, child_id])
    );
    assert_eq!(set.transaction_count(), 1);
    assert!(set.contains(&survivor_id.mined_id()));
    assert!(set.created_output(&output).is_none());
    assert!(set.transaction_dependencies().dependencies().is_empty());
    assert!(set.transaction_dependencies().dependents().is_empty());
    assert_eq!(set.total_serialized_size(), survivor_size);
    assert_eq!(set.total_cost(), survivor_cost);
    assert!(set
        .remove_all([parent_id.mined_id(), child_id.mined_id(), missing])
        .is_empty());
    assert_eq!(set.total_serialized_size(), survivor_size);
    assert_eq!(set.total_cost(), survivor_cost);
}

#[test]
fn mined_parent_preserves_unmined_child() {
    let template = template();
    let parent = transaction(&template, 0, None);
    let parent_id = parent.transaction.id;
    let child = transaction(&template, 1, Some(parent_id.mined_id()));
    let child_id = child.transaction.id;
    let child_size = child.transaction.size;
    let child_cost = child.cost();
    let output = transparent::OutPoint::from_usize(parent_id.mined_id(), 0);
    let mut pending = PendingOutputs::default();
    let mut set = VerifiedSet::default();
    set.insert(parent, vec![], &mut pending, None)
        .expect("a fresh set has no spend conflicts");
    set.insert(child, vec![output], &mut pending, None)
        .expect("the parent output is available");
    let mined_ids = HashSet::from([parent_id.mined_id()]);
    set.clear_mined_dependencies(&mined_ids);
    assert_eq!(set.remove_all(mined_ids), HashSet::from([parent_id]));
    assert_eq!(set.transaction_count(), 1);
    assert!(set.contains(&child_id.mined_id()));
    assert!(set.created_output(&output).is_none());
    assert!(set.transaction_dependencies().dependencies().is_empty());
    assert_eq!(set.total_serialized_size(), child_size);
    assert_eq!(set.total_cost(), child_cost);
}

/// Storage reports every removal, including dependents evicted with their parent.
#[test]
fn storage_reports_all_evicted_membership_ids() {
    let _init_guard = zebra_test::init();
    let template = Network::Mainnet
        .unmined_transactions_in_blocks(..=10)
        .find(|tx| {
            let outputs = tx.transaction.transaction.outputs();
            !outputs.is_empty() && outputs.iter().all(|output| !output.is_dust())
        })
        .expect("early mainnet fixtures contain a non-dust transaction");
    let parent = transaction(&template, 1, None);
    let parent_id = parent.transaction.id;
    let child = transaction(&template, 2, Some(parent_id.mined_id()));
    let child_id = child.transaction.id;
    let outpoint = transparent::OutPoint::from_usize(parent_id.mined_id(), 0);
    assert_eq!(parent.eviction_weight(), child.eviction_weight());

    // Retry until the equal-weight parent is selected.
    for _ in 0..64 {
        let mut storage = super::super::Storage::new(&crate::components::mempool::config::Config {
            tx_cost_limit: parent.cost(),
            ..Default::default()
        });
        let mut evicted = HashSet::new();
        storage
            .insert(parent.clone(), vec![], None, &mut evicted)
            .expect("the parent has no conflicts and the mempool has room");
        assert!(evicted.is_empty());
        let _ = storage.insert(child.clone(), vec![outpoint], None, &mut evicted);
        assert!(!storage.contains_transaction_exact(&child_id.mined_id()));

        if !storage.contains_transaction_exact(&parent_id.mined_id()) {
            assert_eq!(evicted, HashSet::from([parent_id, child_id]));
            return;
        }
        assert_eq!(evicted, HashSet::from([child_id]));
    }
    panic!("equal-weight random eviction should select the parent within 64 attempts");
}
