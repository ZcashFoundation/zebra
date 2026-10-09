//! Incremental metric and bounded gossip-drain behavior.

use super::*;
use zebra_chain::parameters::Network;

fn expected_totals(set: &VerifiedSet) -> MetricTotals {
    let mut totals = MetricTotals::default();
    for tx in set.transactions.values() {
        totals.insert(tx);
    }
    totals
}

#[test]
fn metrics_and_pending_ids_follow_mutations() {
    let _guard = zebra_test::init();
    let mut set = VerifiedSet::default();
    let mut outputs = PendingOutputs::default();
    let mut transactions: Vec<_> = Network::Mainnet
        .unmined_transactions_in_blocks(..=10)
        .collect();
    assert!(transactions.len() >= 10);
    let weights = [0.1, 0.2, 0.4, 0.6, 0.8, 1.0, 1.1, 2.1, 3.1];
    for (tx, weight) in transactions.iter_mut().zip(weights) {
        tx.fee_weight_ratio = weight;
        tx.conventional_actions = 7;
        tx.unpaid_actions = if weight < 1.0 { 3 } else { 0 };
        set.insert(tx.clone(), vec![], &mut outputs, None).unwrap();
        assert_eq!(set.metric_totals, expected_totals(&set));
        assert_eq!(set.pending_gossip.len(), set.transaction_count());
    }

    // Replacing a spendless transaction does not double its metric contribution.
    let mut replacement = transactions[0].clone();
    assert_eq!(
        replacement
            .transaction
            .transaction
            .spent_outpoints()
            .count(),
        0
    );
    replacement.fee_weight_ratio = 3.5;
    replacement.unpaid_actions = 0;
    set.insert(replacement, vec![], &mut outputs, None).unwrap();
    assert_eq!(set.metric_totals, expected_totals(&set));

    let before = expected_totals(&set);
    let missing = transparent::OutPoint::from_usize(transactions[9].transaction.id.mined_id(), 0);
    assert!(set
        .insert(transactions[9].clone(), vec![missing], &mut outputs, None)
        .is_err());
    assert_eq!(set.metric_totals, before);

    assert!(set.take_pending_gossip(0).is_empty());
    let drained = set.take_pending_gossip(2);
    assert_eq!(drained.len(), 2);
    assert!(drained.is_disjoint(&set.pending_gossip));
    assert_eq!(set.pending_gossip.len(), set.transaction_count() - 2);

    // Dependency removal accounts for the child as well as the parent.
    let parent = transactions[1].transaction.id.mined_id();
    let child = transactions[2].transaction.id.mined_id();
    set.transaction_dependencies
        .add(child, vec![transparent::OutPoint::from_usize(parent, 0)]);
    let removed = set.remove(&parent);
    assert_eq!(removed.len(), 2);
    for tx in removed {
        assert!(!set.pending_gossip.contains(&tx.transaction.id));
    }
    assert_eq!(set.metric_totals, expected_totals(&set));

    set.remove_all_that(|tx| tx.fee_weight_ratio < 1.0);
    assert_eq!(set.metric_totals, expected_totals(&set));
    for evicted in set.evict_one() {
        assert!(!set.pending_gossip.contains(&evicted.transaction.id));
    }
    assert_eq!(set.metric_totals, expected_totals(&set));
    set.clear();
    assert_eq!(set.metric_totals, MetricTotals::default());
    assert!(set.pending_gossip.is_empty());
}

#[test]
fn metric_buckets_match_thresholds() {
    let _guard = zebra_test::init();
    let mut tx = Network::Mainnet
        .unmined_transactions_in_blocks(..)
        .next()
        .unwrap();
    for (weight, expected) in [
        (0.0, (0, Some(0))),
        (0.2, (0, Some(1))),
        (0.4, (0, Some(2))),
        (0.6, (0, Some(3))),
        (0.8, (0, Some(4))),
        (1.0, (1, None)),
        (2.0, (2, None)),
        (3.0, (3, None)),
        (3.1, (4, None)),
    ] {
        tx.fee_weight_ratio = weight;
        assert_eq!(MetricTotals::buckets(&tx), expected);
    }
}
