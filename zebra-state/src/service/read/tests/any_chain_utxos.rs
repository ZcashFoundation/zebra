//! Batched transparent output reads match the ordinary `AwaitUtxos` lookup.

use std::sync::Arc;

use zebra_chain::{
    block::Block,
    serialization::ZcashDeserializeInto,
    transparent::{self, Input, Output, Script},
};

use crate::{
    service::read::any_chain_utxos,
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    CheckpointVerifiedBlock,
};

#[test]
fn prefetch_finds_outputs_and_omits_finalized_spends() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();
    let (mut finalized, non_finalized, _) = new_state_with_mainnet_genesis();
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let mut parent = Block::clone(&genesis.make_fake_child());
    parent.transactions[0] = Arc::new(
        parent.transactions[0]
            .as_ref()
            .clone()
            .with_transparent_outputs(vec![Output::new(
                1.try_into().unwrap(),
                Script::new(&[0x51]),
            )]),
    );
    let parent = Arc::new(parent);
    finalized.commit_finalized_direct(
        CheckpointVerifiedBlock::from(parent.clone()).into(),
        None,
        "prefetch parent",
    )?;
    let outpoint = transparent::OutPoint {
        hash: parent.transactions[0].hash(),
        index: 0,
    };
    assert!(any_chain_utxos(&non_finalized, &finalized.db, &[outpoint]).contains_key(&outpoint));

    let missing = transparent::OutPoint {
        index: 1,
        ..outpoint
    };
    let mut over_limit = vec![missing; 16_384];
    over_limit.push(outpoint);
    assert!(
        any_chain_utxos(&non_finalized, &finalized.db, &over_limit).is_empty(),
        "outputs after the count cap must use AwaitUtxos"
    );

    let spend = genesis.transactions[0]
        .as_ref()
        .clone()
        .with_transparent_inputs(vec![Input::PrevOut {
            outpoint,
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        }])
        .with_transparent_outputs(Vec::new());
    let mut spent_parent = Block::clone(&parent.make_fake_child());
    spent_parent.transactions.push(Arc::new(spend));
    let spent_parent = Arc::new(spent_parent);
    finalized.commit_finalized_direct(
        CheckpointVerifiedBlock::from(spent_parent).into(),
        None,
        "prefetch spend",
    )?;
    assert!(any_chain_utxos(&non_finalized, &finalized.db, &[outpoint]).is_empty());
    Ok(())
}
