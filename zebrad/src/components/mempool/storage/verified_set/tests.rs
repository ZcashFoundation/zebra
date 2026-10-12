//! Regression tests for incremental verified-set metrics.

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
                set.remove_all_that(|tx| tx.transaction.id.mined_id().0[0] % 2 == 0);
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
                assert!(set.evict_one().is_some());
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
    assert_eq!(set.remove_all_that(|_| true).len(), chain.len());
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
