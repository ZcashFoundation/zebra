//! Regression tests for coherent output metadata during finalization.

use std::{cell::RefCell, sync::Arc};

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto, transparent};

use crate::{
    service::{non_finalized_state::Chain, read::unspent_output},
    tests::setup::new_state_with_mainnet_genesis,
    CheckpointVerifiedBlock,
};

thread_local! {
    pub(in crate::service::read) static AFTER_TIP_CAPTURE: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[test]
fn unspent_output_retries_when_output_is_above_captured_tip() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();
    let (mut finalized, _, _) = new_state_with_mainnet_genesis();
    let db = finalized.db.clone();
    let block: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&1]
        .zcash_deserialize_into()
        .unwrap();
    let outpoint = transparent::OutPoint {
        hash: block.transactions[0].hash(),
        index: 0,
    };
    let committed = block.clone();
    AFTER_TIP_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            finalized
                .commit_finalized_direct(
                    CheckpointVerifiedBlock::from(committed).into(),
                    None,
                    "output tip race test",
                )
                .unwrap();
        }));
    });

    let output = unspent_output(None::<Arc<Chain>>, &db, outpoint)?.unwrap();
    assert_eq!(output.utxo.output, block.transactions[0].outputs()[0]);
    assert_eq!(output.utxo.height, block.coinbase_height().unwrap());
    assert_eq!(output.confirmations, 1);
    assert_eq!(output.best_chain_tip_hash, block.hash());
    AFTER_TIP_CAPTURE.with(|hook| assert!(hook.borrow().is_none()));
    Ok(())
}
