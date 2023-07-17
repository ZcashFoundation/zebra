//! Fixed test vectors for the non-finalized state.

use std::sync::Arc;

use zebra_chain::{
    amount::NonNegative,
    block::{Block, Height},
    history_tree::NonEmptyHistoryTree,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    value_balance::ValueBalance,
};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    service::{
        finalized_state::FinalizedState,
        non_finalized_state::{Chain, NonFinalizedState},
    },
    tests::FakeChainHelper,
    Config,
};

#[test]
fn construct_empty() {
    let _init_guard = zebra_test::init();
    let _chain = Chain::new(
        Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::zero(),
    );
}

#[test]
fn construct_single() -> Result<()> {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;

    let mut chain = Chain::new(
        Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );

    chain = chain.push(block.prepare().test_with_zero_spent_utxos())?;

    assert_eq!(1, chain.blocks.len());

    Ok(())
}

#[test]
fn construct_many() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let initial_height = block
        .coinbase_height()
        .expect("Block 434873 should have its height in its coinbase tx.");
    let mut blocks = vec![];

    while blocks.len() < 100 {
        let next_block = block.make_fake_child();
        blocks.push(block);
        block = next_block;
    }

    let mut chain = Chain::new(
        Network::Mainnet,
        (initial_height - 1).expect("Initial height should be at least 1."),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );

    for block in blocks {
        chain = chain.push(block.prepare().test_with_zero_spent_utxos())?;
    }

    assert_eq!(100, chain.blocks.len());

    Ok(())
}

#[test]
fn ord_matches_work() -> Result<()> {
    let _init_guard = zebra_test::init();
    let less_block = zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .set_work(1);
    let more_block = less_block.clone().set_work(10);

    let mut lesser_chain = Chain::new(
        Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    lesser_chain = lesser_chain.push(less_block.prepare().test_with_zero_spent_utxos())?;

    let mut bigger_chain = Chain::new(
        Network::Mainnet,
        Height(0),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::zero(),
    );
    bigger_chain = bigger_chain.push(more_block.prepare().test_with_zero_spent_utxos())?;

    assert!(bigger_chain > lesser_chain);

    Ok(())
}

#[test]
fn best_chain_wins() -> Result<()> {
    let _init_guard = zebra_test::init();

    best_chain_wins_for_network(Network::Mainnet)?;
    best_chain_wins_for_network(Network::Testnet)?;

    Ok(())
}

fn best_chain_wins_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        // Since the brand new FinalizedState below will pass a None history tree
        // to the NonFinalizedState, we must use pre-Heartwood blocks since
        // they won't trigger the history tree update in the NonFinalizedState.
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into()?
        }
    };

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let expected_hash = block2.hash();

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    state.commit_new_chain(block2.prepare(), &finalized_state)?;
    state.commit_new_chain(child.prepare(), &finalized_state)?;

    let best_chain = state.best_chain().unwrap();
    assert!(best_chain.height_by_hash.contains_key(&expected_hash));

    Ok(())
}

#[test]
fn finalize_pops_from_best_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    finalize_pops_from_best_chain_for_network(Network::Mainnet)?;
    finalize_pops_from_best_chain_for_network(Network::Testnet)?;

    Ok(())
}

fn finalize_pops_from_best_chain_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        // Since the brand new FinalizedState below will pass a None history tree
        // to the NonFinalizedState, we must use pre-Heartwood blocks since
        // they won't trigger the history tree update in the NonFinalizedState.
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into()?
        }
    };

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(child.prepare(), &finalized_state)?;

    let finalized = state.finalize().inner_block();

    assert_eq!(block1, finalized);

    let finalized = state.finalize().inner_block();
    assert_eq!(block2, finalized);

    assert!(state.best_chain().is_none());

    Ok(())
}

#[test]
// This test gives full coverage for `take_chain_if`
fn commit_block_extending_best_chain_doesnt_drop_worst_chains() -> Result<()> {
    let _init_guard = zebra_test::init();

    commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(Network::Mainnet)?;
    commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(Network::Testnet)?;

    Ok(())
}

fn commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(
    network: Network,
) -> Result<()> {
    let block1: Arc<Block> = match network {
        // Since the brand new FinalizedState below will pass a None history tree
        // to the NonFinalizedState, we must use pre-Heartwood blocks since
        // they won't trigger the history tree update in the NonFinalizedState.
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into()?
        }
    };

    let block2 = block1.make_fake_child().set_work(10);
    let child1 = block1.make_fake_child().set_work(1);
    let child2 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    assert_eq!(0, state.chain_set.len());
    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(block2.prepare(), &finalized_state)?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(child1.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());
    state.commit_block(child2.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    Ok(())
}

#[test]
fn shorter_chain_can_be_best_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    shorter_chain_can_be_best_chain_for_network(Network::Mainnet)?;
    shorter_chain_can_be_best_chain_for_network(Network::Testnet)?;

    Ok(())
}

fn shorter_chain_can_be_best_chain_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        // Since the brand new FinalizedState below will pass a None history tree
        // to the NonFinalizedState, we must use pre-Heartwood blocks since
        // they won't trigger the history tree update in the NonFinalizedState.
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into()?
        }
    };

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_state)?;
    state.commit_block(short_chain_block.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(2, state.best_chain_len());

    Ok(())
}

#[test]
fn longer_chain_with_more_work_wins() -> Result<()> {
    let _init_guard = zebra_test::init();

    longer_chain_with_more_work_wins_for_network(Network::Mainnet)?;
    longer_chain_with_more_work_wins_for_network(Network::Testnet)?;

    Ok(())
}

fn longer_chain_with_more_work_wins_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        // Since the brand new FinalizedState below will pass a None history tree
        // to the NonFinalizedState, we must use pre-Heartwood blocks since
        // they won't trigger the history tree update in the NonFinalizedState.
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into()?
        }
    };

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);
    let long_chain_block3 = long_chain_block2.make_fake_child().set_work(1);
    let long_chain_block4 = long_chain_block3.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block3.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block4.prepare(), &finalized_state)?;
    state.commit_block(short_chain_block.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(5, state.best_chain_len());

    Ok(())
}

#[test]
fn equal_length_goes_to_more_work() -> Result<()> {
    let _init_guard = zebra_test::init();

    equal_length_goes_to_more_work_for_network(Network::Mainnet)?;
    equal_length_goes_to_more_work_for_network(Network::Testnet)?;

    Ok(())
}
fn equal_length_goes_to_more_work_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        // Since the brand new FinalizedState below will pass a None history tree
        // to the NonFinalizedState, we must use pre-Heartwood blocks since
        // they won't trigger the history tree update in the NonFinalizedState.
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_653599_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_583999_BYTES.zcash_deserialize_into()?
        }
    };

    let less_work_child = block1.make_fake_child().set_work(1);
    let more_work_child = block1.make_fake_child().set_work(3);
    let expected_hash = more_work_child.hash();

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(less_work_child.prepare(), &finalized_state)?;
    state.commit_block(more_work_child.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    let tip_hash = state.best_tip().unwrap().1;
    assert_eq!(expected_hash, tip_hash);

    Ok(())
}

#[test]
fn history_tree_is_updated() -> Result<()> {
    history_tree_is_updated_for_network_upgrade(Network::Mainnet, NetworkUpgrade::Heartwood)?;
    history_tree_is_updated_for_network_upgrade(Network::Testnet, NetworkUpgrade::Heartwood)?;
    // TODO: we can't test other upgrades until we have a method for creating a FinalizedState
    // with a HistoryTree.
    Ok(())
}

fn history_tree_is_updated_for_network_upgrade(
    network: Network,
    network_upgrade: NetworkUpgrade,
) -> Result<()> {
    let blocks = match network {
        Network::Mainnet => &*zebra_test::vectors::MAINNET_BLOCKS,
        Network::Testnet => &*zebra_test::vectors::TESTNET_BLOCKS,
    };
    let height = network_upgrade.activation_height(network).unwrap().0;

    let prev_block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    state
        .commit_new_chain(prev_block.clone().prepare(), &finalized_state)
        .unwrap();

    let chain = state.best_chain().unwrap();
    if network_upgrade == NetworkUpgrade::Heartwood {
        assert!(
            chain.history_block_commitment_tree().as_ref().is_none(),
            "history tree must not exist yet"
        );
    } else {
        assert!(
            chain.history_block_commitment_tree().as_ref().is_some(),
            "history tree must already exist"
        );
    }

    // The Heartwood activation block has an all-zero commitment
    let activation_block = prev_block.make_fake_child().set_block_commitment([0u8; 32]);

    state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap();

    let chain = state.best_chain().unwrap();
    assert!(
        chain.history_block_commitment_tree().as_ref().is_some(),
        "history tree must have been (re)created"
    );
    assert_eq!(
        chain
            .history_block_commitment_tree()
            .as_ref()
            .as_ref()
            .unwrap()
            .size(),
        1,
        "history tree must have a single node"
    );

    // To fix the commitment in the next block we must recreate the history tree
    let tree = NonEmptyHistoryTree::from_block(
        Network::Mainnet,
        activation_block.clone(),
        &chain.sapling_note_commitment_tree().root(),
        &chain.orchard_note_commitment_tree().root(),
    )
    .unwrap();

    let next_block = activation_block
        .make_fake_child()
        .set_block_commitment(tree.hash().into());

    state
        .commit_block(next_block.prepare(), &finalized_state)
        .unwrap();

    assert!(
        state
            .best_chain()
            .unwrap()
            .history_block_commitment_tree()
            .as_ref()
            .is_some(),
        "history tree must still exist"
    );

    Ok(())
}

#[test]
fn commitment_is_validated() {
    commitment_is_validated_for_network_upgrade(Network::Mainnet, NetworkUpgrade::Heartwood);
    commitment_is_validated_for_network_upgrade(Network::Testnet, NetworkUpgrade::Heartwood);
    // TODO: we can't test other upgrades until we have a method for creating a FinalizedState
    // with a HistoryTree.
}

fn commitment_is_validated_for_network_upgrade(network: Network, network_upgrade: NetworkUpgrade) {
    let blocks = match network {
        Network::Mainnet => &*zebra_test::vectors::MAINNET_BLOCKS,
        Network::Testnet => &*zebra_test::vectors::TESTNET_BLOCKS,
    };
    let height = network_upgrade.activation_height(network).unwrap().0;

    let prev_block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        None,
    );

    state
        .commit_new_chain(prev_block.clone().prepare(), &finalized_state)
        .unwrap();

    // The Heartwood activation block must have an all-zero commitment.
    // Test error return when committing the block with the wrong commitment
    let activation_block = prev_block.make_fake_child();
    let err = state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap_err();
    match err {
        crate::ValidateContextError::InvalidBlockCommitment(
            zebra_chain::block::CommitmentError::InvalidChainHistoryActivationReserved { .. },
        ) => {},
        _ => panic!("Error must be InvalidBlockCommitment::InvalidChainHistoryActivationReserved instead of {err:?}"),
    };

    // Test committing the Heartwood activation block with the correct commitment
    let activation_block = activation_block.set_block_commitment([0u8; 32]);
    state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap();

    // To fix the commitment in the next block we must recreate the history tree
    let chain = state.best_chain().unwrap();
    let tree = NonEmptyHistoryTree::from_block(
        Network::Mainnet,
        activation_block.clone(),
        &chain.sapling_note_commitment_tree().root(),
        &chain.orchard_note_commitment_tree().root(),
    )
    .unwrap();

    // Test committing the next block with the wrong commitment
    let next_block = activation_block.make_fake_child();
    let err = state
        .commit_block(next_block.clone().prepare(), &finalized_state)
        .unwrap_err();
    match err {
        crate::ValidateContextError::InvalidBlockCommitment(
            zebra_chain::block::CommitmentError::InvalidChainHistoryRoot { .. },
        ) => {}
        _ => panic!(
            "Error must be InvalidBlockCommitment::InvalidChainHistoryRoot instead of {err:?}"
        ),
    };

    // Test committing the next block with the correct commitment
    let next_block = next_block.set_block_commitment(tree.hash().into());
    state
        .commit_block(next_block.prepare(), &finalized_state)
        .unwrap();
}
