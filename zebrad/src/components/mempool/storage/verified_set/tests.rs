use super::*;
use std::collections::HashSet;
use zebra_chain::{
    amount::Amount,
    transaction::{self, LockTime, Transaction, UnminedTx, UnminedTxId, VerifiedUnminedTx},
    transparent,
};

fn dummy_output(tag: u8) -> transparent::Output {
    transparent::Output {
        value: Amount::try_from(1_000_000).expect("valid amount"),
        lock_script: transparent::Script::new(&[tag]),
    }
}

fn dummy_transaction(tag: u8) -> Transaction {
    Transaction::V1 {
        inputs: Vec::new(),
        outputs: vec![dummy_output(tag)],
        lock_time: LockTime::unlocked(),
    }
}

fn dummy_verified_unmined_tx(tag: u8) -> VerifiedUnminedTx {
    let unmined = UnminedTx::from(dummy_transaction(tag));

    VerifiedUnminedTx {
        transaction: unmined,
        miner_fee: Amount::try_from(1_000).expect("valid amount"),
        sigops: 0,
        conventional_actions: 0,
        unpaid_actions: 0,
        fee_weight_ratio: 0.0,
        time: None,
        height: None,
    }
}

fn insert_dummy_tx(
    set: &mut VerifiedSet,
    tx: VerifiedUnminedTx,
) -> (transaction::Hash, UnminedTxId) {
    let unmined_id = tx.transaction.id;
    let tx_hash = unmined_id.mined_id();

    set.transactions_serialized_size += tx.transaction.size;
    set.total_cost += tx.cost();
    set.transactions.insert(tx_hash, tx);

    (tx_hash, unmined_id)
}

#[test]
fn verified_set_remove_all_that_with_dependencies() {
    let _init_guard = zebra_test::init();
    let mut set = VerifiedSet::default();

    let (parent_hash, parent_id) = insert_dummy_tx(&mut set, dummy_verified_unmined_tx(0x01));
    let (child_hash, child_id) = insert_dummy_tx(&mut set, dummy_verified_unmined_tx(0x02));

    set.transaction_dependencies.add(
        child_hash,
        vec![transparent::OutPoint::from_usize(parent_hash, 0)],
    );

    let expected: HashSet<_> = [parent_id, child_id].into_iter().collect();

    let removed = set.remove_all_that(|tx| {
        let mined_id = tx.transaction.id.mined_id();
        mined_id == parent_hash || mined_id == child_hash
    });

    assert_eq!(removed, expected);
    assert!(set.transactions.is_empty());
    assert!(set.transaction_dependencies.dependents().is_empty());
    assert_eq!(set.transactions_serialized_size, 0);
    assert_eq!(set.total_cost, 0);
}
