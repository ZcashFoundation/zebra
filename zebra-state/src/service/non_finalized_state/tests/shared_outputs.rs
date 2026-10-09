//! Large output ownership across chain snapshots, forks, rollback and finalization.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare, service::non_finalized_state::Chain, tests::FakeChainHelper,
    ContextuallyVerifiedBlock,
};

fn funded_chain() -> (Chain, Arc<Block>, transparent::OutPoint) {
    let mut block = zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("the historical block vector deserializes");
    let block_mut = Arc::make_mut(&mut block);
    block_mut.transactions.truncate(1);
    let coinbase = Arc::make_mut(&mut block_mut.transactions[0]);
    let mut outputs = coinbase.outputs().to_vec();
    outputs[0].lock_script = transparent::Script::new(&[0x51; 10_000]);
    coinbase.set_outputs(outputs);
    let outpoint = transparent::OutPoint {
        hash: block.transactions[0].hash(),
        index: 0,
    };
    let chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    )
    .push(block.clone().prepare().test_with_zero_spent_utxos())
    .expect("the funding fixture passes chain update checks");
    (chain, block, outpoint)
}

#[test]
fn snapshots_share_outputs_without_sharing_spend_or_branch_membership() {
    let (parent, block, outpoint) = funded_chain();
    let mut child = block.make_fake_child();
    let spend = Transaction::test_v4(
        vec![transparent::Input::PrevOut {
            outpoint,
            unlock_script: transparent::Script::new(&[]),
            sequence: u32::MAX,
        }],
        vec![block.transactions[0].outputs()[0].clone()],
        LockTime::unlocked(),
        Height(0),
    );
    let spend_outpoint = transparent::OutPoint {
        hash: spend.hash(),
        index: 0,
    };
    Arc::make_mut(&mut child).transactions.push(Arc::new(spend));

    // Exercise state updates directly. Semantic checks and coinbase maturity
    // are covered by the existing service tests.
    let contextual = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        child.clone().prepare(),
        HashMap::from([(outpoint, parent.created_utxos[&outpoint].as_ref().clone())]),
        DeferredPoolBalanceChange::zero(),
        &Network::Mainnet,
        parent.chain_value_pools,
    )
    .expect("the supplied spent output gives the fixture's value balance");
    let spent_chain = parent
        .clone()
        .push(contextual)
        .expect("the child updates the chain");
    assert!(Arc::ptr_eq(
        &parent.created_utxos[&outpoint],
        &spent_chain.created_utxos[&outpoint]
    ));
    assert!(parent.unspent_utxos().contains_key(&outpoint));
    assert!(!spent_chain.unspent_utxos().contains_key(&outpoint));
    assert!(spent_chain.created_utxo(&outpoint).is_some());
    assert!(parent.created_utxo(&spend_outpoint).is_none());
    assert!(spent_chain.created_utxo(&spend_outpoint).is_some());

    #[cfg(feature = "indexer")]
    {
        let spend = crate::Spend::OutPoint(outpoint);
        assert_eq!(parent.spending_transaction_hash(&spend), None);
        assert_eq!(
            spent_chain.spending_transaction_hash(&spend),
            Some(spend_outpoint.hash)
        );
    }

    let fork = spent_chain
        .fork(block.hash())
        .expect("the parent is in the chain");
    assert!(fork.eq_internal_state(&parent));
    assert!(Arc::ptr_eq(
        &fork.created_utxos[&outpoint],
        &parent.created_utxos[&outpoint]
    ));

    let (mut rolled_back, invalidated) = spent_chain
        .invalidate_block(child.hash())
        .expect("the child is in the chain");
    assert!(rolled_back.eq_internal_state(&parent));
    assert!(rolled_back.unspent_utxos().contains_key(&outpoint));
    assert!(rolled_back.created_utxo(&spend_outpoint).is_none());
    assert!(!spent_chain.unspent_utxos().contains_key(&outpoint));
    for block in invalidated {
        rolled_back = rolled_back
            .push(Arc::unwrap_or_clone(block))
            .expect("the invalidated child can be replayed");
    }
    assert!(rolled_back.eq_internal_state(&spent_chain));
}

#[test]
fn finalization_and_owned_queries_preserve_other_snapshot_payloads() {
    let (mut chain, block, outpoint) = funded_chain();
    let snapshot = chain.clone();
    let payload = Arc::downgrade(&chain.created_utxos[&outpoint]);
    let expected = chain.created_utxo(&outpoint).expect("the output exists");
    let mut returned_output = snapshot.created_utxo(&outpoint).expect("the output exists");
    returned_output.output.lock_script = transparent::Script::new(&[0x52]);
    assert_eq!(snapshot.created_utxo(&outpoint), Some(expected.clone()));

    let mut returned = snapshot.unspent_utxos();
    returned
        .get_mut(&outpoint)
        .expect("the output is unspent")
        .utxo
        .output
        .lock_script = transparent::Script::new(&[0x52]);
    assert_eq!(snapshot.created_utxo(&outpoint), Some(expected.clone()));

    chain = chain
        .push(
            block
                .make_fake_child()
                .prepare()
                .test_with_zero_spent_utxos(),
        )
        .expect("the child updates the chain");
    let (finalized, _) = chain.pop_root();
    assert_eq!(finalized.hash, block.hash());
    assert!(chain.created_utxo(&outpoint).is_none());
    assert_eq!(snapshot.created_utxo(&outpoint), Some(expected));
    assert!(
        payload.upgrade().is_some(),
        "the old snapshot still owns the payload"
    );
    drop(snapshot);
    assert!(
        payload.upgrade().is_none(),
        "the shared payload is released with its last snapshot"
    );
}

#[test]
fn snapshots_share_block_records_until_their_last_owner_drops() {
    let (chain, block, _) = funded_chain();
    let mut chain = chain
        .push(
            block
                .make_fake_child()
                .prepare()
                .test_with_zero_spent_utxos(),
        )
        .expect("the child updates the chain");
    let snapshot = chain.clone();
    assert!(chain
        .blocks
        .values()
        .zip(snapshot.blocks.values())
        .all(|(current, retained)| Arc::ptr_eq(current, retained)));

    let root = Arc::downgrade(chain.blocks.values().next().expect("the chain has blocks"));
    let (finalized, _) = chain.pop_root();
    assert_eq!(finalized.hash, block.hash());
    assert_eq!(snapshot.non_finalized_root_hash(), block.hash());
    drop(finalized);
    assert!(
        root.upgrade().is_some(),
        "the snapshot still owns the popped root record"
    );
    drop(snapshot);
    assert!(
        root.upgrade().is_none(),
        "the record is released with its last owner"
    );
}
