//! Regression tests for coherent transaction metadata during finalization.

use std::{
    cell::RefCell,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
};

use zebra_chain::{
    block::{Block, Height},
    serialization::ZcashDeserializeInto,
};

use crate::{
    arbitrary::Prepare,
    response::AnyTx,
    service::{
        non_finalized_state::Chain,
        read::{any_transaction, mined_transaction},
    },
    tests::setup::new_state_with_mainnet_genesis,
    CheckpointVerifiedBlock,
};

thread_local! {
    pub(in crate::service::read) static AFTER_TIP_CAPTURE: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[test]
fn transaction_queries_use_advanced_finalized_tip_for_multiple_transactions(
) -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();
    let (mut finalized, mut non_finalized, _) = new_state_with_mainnet_genesis();
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .range(1..=5)
        .map(|(_, bytes)| bytes.zcash_deserialize_into().unwrap())
        .collect();

    non_finalized.commit_new_chain(blocks[0].clone().prepare(), &finalized.db)?;
    for block in &blocks[1..3] {
        non_finalized.commit_block(block.clone().prepare(), &finalized.db)?;
    }
    let cached_chain = non_finalized.best_chain().unwrap();
    for block in &blocks {
        finalized.commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "transaction tip test",
        )?;
    }
    assert_eq!(cached_chain.non_finalized_tip_height(), Height(3));
    assert_eq!(finalized.db.finalized_tip_height(), Some(Height(5)));

    for block in blocks.iter().rev() {
        let hash = block.transactions[0].hash();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mined = mined_transaction(Some(&cached_chain), &finalized.db, hash).unwrap();
            let any = any_transaction(std::iter::once(cached_chain), &finalized.db, hash).unwrap();
            (mined, any)
        }));
        let (mined, any) =
            result.expect("an advanced finalized tip must not underflow confirmations");
        let mined = mined.expect("the transaction is in the best chain");
        let AnyTx::Mined(any) = any.expect("the transaction is in any chain") else {
            panic!("a finalized transaction must not be reported on a stale side chain");
        };

        for transaction in [mined, any] {
            assert_eq!(transaction.tx, block.transactions[0]);
            assert_eq!(transaction.height, block.coinbase_height().unwrap());
            assert_eq!(
                transaction.confirmations,
                1 + Height(5).0 - block.coinbase_height().unwrap().0,
            );
            assert_eq!(transaction.best_chain_tip_hash, blocks[4].hash());
        }
    }

    Ok(())
}

#[test]
fn transaction_queries_retry_when_the_finalized_tip_changes() -> Result<(), crate::BoxError> {
    for any_chain in [false, true] {
        let _init_guard = zebra_test::init();
        let (mut finalized, _, _) = new_state_with_mainnet_genesis();
        let db = finalized.db.clone();
        let block: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&1]
            .zcash_deserialize_into()
            .unwrap();
        let hash = block.transactions[0].hash();
        let committed = block.clone();
        AFTER_TIP_CAPTURE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                finalized
                    .commit_finalized_direct(
                        CheckpointVerifiedBlock::from(committed).into(),
                        None,
                        "transaction tip race test",
                    )
                    .unwrap();
            }));
        });

        let transaction = if any_chain {
            let AnyTx::Mined(transaction) =
                any_transaction(std::iter::empty::<&Arc<Chain>>(), &db, hash)?
                    .expect("the transaction committed during the query is found")
            else {
                panic!("a finalized transaction is in the best chain");
            };
            transaction
        } else {
            mined_transaction(None::<Arc<Chain>>, &db, hash)?
                .expect("the transaction committed during the query is found")
        };
        assert_eq!(transaction.height, Height(1));
        assert_eq!(transaction.confirmations, 1);
        assert_eq!(transaction.best_chain_tip_hash, block.hash());
        AFTER_TIP_CAPTURE.with(|hook| assert!(hook.borrow().is_none()));
    }

    Ok(())
}

#[test]
fn transaction_queries_reject_disagreeing_finalized_and_cached_chains(
) -> Result<(), crate::BoxError> {
    use crate::tests::FakeChainHelper;

    let _init_guard = zebra_test::init();
    let (mut finalized, mut non_finalized, genesis) = new_state_with_mainnet_genesis();
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .range(1..=5)
        .map(|(_, bytes)| bytes.zcash_deserialize_into().unwrap())
        .collect();
    non_finalized.commit_new_chain(blocks[0].clone().prepare(), &finalized.db)?;
    for block in &blocks[1..] {
        non_finalized.commit_block(block.clone().prepare(), &finalized.db)?;
    }
    let cached_chain = non_finalized.best_chain().unwrap();

    let mut alternate = genesis
        .block
        .make_fake_child()
        .set_block_commitment([0x42; 32]);
    let mut alternates = vec![alternate.clone()];
    for _ in 1..4 {
        alternate = alternate.make_fake_child();
        alternates.push(alternate.clone());
    }
    for block in &alternates {
        finalized.commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "transaction fork test",
        )?;
    }
    assert_eq!(cached_chain.non_finalized_tip_height(), Height(5));
    assert_eq!(finalized.db.finalized_tip_height(), Some(Height(4)));

    let hash = alternates[3].transactions[0].hash();
    let mined = mined_transaction(Some(&cached_chain), &finalized.db, hash);
    assert!(
        mined.is_err(),
        "a finalized-only transaction must not use an incompatible cached tip"
    );
    let any = any_transaction(std::iter::once(cached_chain), &finalized.db, hash);
    assert!(
        any.is_err(),
        "an any-chain transaction must not use an incompatible cached tip"
    );

    Ok(())
}
