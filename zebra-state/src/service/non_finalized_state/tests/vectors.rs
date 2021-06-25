use std::sync::Arc;

use zebra_chain::{
    block::Block,
    mmr::{HistoryTree, HistoryTreeError},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_test::prelude::*;

use crate::{
    service::non_finalized_state::{Chain, NonFinalizedState},
    tests::{FakeChainHelper, Prepare},
};

use self::assert_eq;

/// Make a history tree for the given block givens the history tree of its parent.
fn make_tree(
    block: Arc<Block>,
    parent_tree: &HistoryTree,
) -> Result<HistoryTree, HistoryTreeError> {
    let mut tree = parent_tree.clone();
    tree.push(block, &Default::default(), None)?;
    Ok(tree)
}

#[test]
fn construct_single() -> Result<()> {
    zebra_test::init();
    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;

    let finalized_tree =
        HistoryTree::new_from_block(Network::Mainnet, block0.clone(), &Default::default(), None)
            .unwrap();

    let block1 = block0
        .make_fake_child()
        .set_commitment(finalized_tree.hash().into());

    let mut chain = Chain::new(finalized_tree);
    chain.push(block1.prepare())?;

    assert_eq!(1, chain.blocks.len());

    Ok(())
}

#[test]
fn construct_many() -> Result<()> {
    zebra_test::init();

    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let finalized_tree =
        HistoryTree::new_from_block(Network::Mainnet, block.clone(), &Default::default(), None)
            .unwrap();
    let mut blocks = vec![];

    let mut tree = finalized_tree.clone();
    while blocks.len() < 100 {
        let next_block = block.make_fake_child().set_commitment(tree.hash().into());
        blocks.push(next_block.clone());
        block = next_block;
        tree = make_tree(block.clone(), &tree)?;
    }

    let mut chain = Chain::new(finalized_tree);

    for block in blocks {
        chain.push(block.prepare())?;
    }

    assert_eq!(100, chain.blocks.len());

    Ok(())
}

#[test]
fn ord_matches_work() -> Result<()> {
    zebra_test::init();
    let block =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
    let finalized_tree =
        HistoryTree::new_from_block(Network::Mainnet, block.clone(), &Default::default(), None)
            .unwrap();

    let less_block = block
        .make_fake_child()
        .set_work(1)
        .set_commitment(finalized_tree.hash().into());
    let more_block = block
        .make_fake_child()
        .set_work(10)
        .set_commitment(finalized_tree.hash().into());

    let mut lesser_chain = Chain::new(finalized_tree.clone());
    lesser_chain.push(less_block.prepare())?;

    let mut bigger_chain = Chain::new(finalized_tree);
    bigger_chain.push(more_block.prepare())?;

    assert!(bigger_chain > lesser_chain);

    Ok(())
}

#[test]
fn best_chain_wins() -> Result<()> {
    zebra_test::init();

    best_chain_wins_for_network(Network::Mainnet)?;
    best_chain_wins_for_network(Network::Testnet)?;

    Ok(())
}

fn best_chain_wins_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };
    let finalized_tree =
        HistoryTree::new_from_block(network, block1.clone(), &Default::default(), None).unwrap();

    let block2 = block1
        .make_fake_child()
        .set_work(10)
        .set_commitment(finalized_tree.hash().into());
    let child = block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(finalized_tree.hash().into());

    let expected_hash = block2.hash();

    let mut state = NonFinalizedState::default();
    state.commit_new_chain(block2.prepare(), finalized_tree.clone())?;
    state.commit_new_chain(child.prepare(), finalized_tree)?;

    let best_chain = state.best_chain().unwrap();
    assert!(best_chain.height_by_hash.contains_key(&expected_hash));

    Ok(())
}

#[test]
fn finalize_pops_from_best_chain() -> Result<()> {
    zebra_test::init();

    finalize_pops_from_best_chain_for_network(Network::Mainnet)?;
    finalize_pops_from_best_chain_for_network(Network::Testnet)?;

    Ok(())
}

fn finalize_pops_from_best_chain_for_network(network: Network) -> Result<()> {
    let block0: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };
    let finalized_tree =
        HistoryTree::new_from_block(network, block0.clone(), &Default::default(), None).unwrap();

    let block1 = block0
        .make_fake_child()
        .set_commitment(finalized_tree.hash().into());
    let block1_tree = make_tree(block1.clone(), &finalized_tree)?;

    let block2 = block1
        .make_fake_child()
        .set_work(10)
        .set_commitment(block1_tree.hash().into());
    let child = block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(block1_tree.hash().into());

    let mut state = NonFinalizedState::default();
    state.commit_new_chain(block1.clone().prepare(), finalized_tree.clone())?;
    state.commit_block(block2.clone().prepare(), &finalized_tree)?;
    state.commit_block(child.prepare(), &finalized_tree)?;

    let finalized = state.finalize();
    assert_eq!(block1, finalized.block);

    let finalized = state.finalize();
    assert_eq!(block2, finalized.block);

    assert!(state.best_chain().is_none());

    Ok(())
}

#[test]
// This test gives full coverage for `take_chain_if`
fn commit_block_extending_best_chain_doesnt_drop_worst_chains() -> Result<()> {
    zebra_test::init();

    commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(Network::Mainnet)?;
    commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(Network::Testnet)?;

    Ok(())
}

fn commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(
    network: Network,
) -> Result<()> {
    let block0: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };
    let finalized_tree =
        HistoryTree::new_from_block(network, block0.clone(), &Default::default(), None).unwrap();

    let block1 = block0
        .make_fake_child()
        .set_commitment(finalized_tree.hash().into());
    let block1_tree = make_tree(block1.clone(), &finalized_tree)?;

    let block2 = block1
        .make_fake_child()
        .set_work(10)
        .set_commitment(block1_tree.hash().into());
    let block2_tree = make_tree(block2.clone(), &block1_tree)?;
    let child1 = block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(block1_tree.hash().into());
    let child2 = block2
        .make_fake_child()
        .set_work(1)
        .set_commitment(block2_tree.hash().into());

    let mut state = NonFinalizedState::default();
    assert_eq!(0, state.chain_set.len());
    state.commit_new_chain(block1.prepare(), finalized_tree.clone())?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(block2.prepare(), &finalized_tree)?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(child1.prepare(), &finalized_tree)?;
    assert_eq!(2, state.chain_set.len());
    state.commit_block(child2.prepare(), &finalized_tree)?;
    assert_eq!(2, state.chain_set.len());

    Ok(())
}

#[test]
fn shorter_chain_can_be_best_chain() -> Result<()> {
    zebra_test::init();

    shorter_chain_can_be_best_chain_for_network(Network::Mainnet)?;
    shorter_chain_can_be_best_chain_for_network(Network::Testnet)?;

    Ok(())
}

fn shorter_chain_can_be_best_chain_for_network(network: Network) -> Result<()> {
    let block0: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };
    let finalized_tree =
        HistoryTree::new_from_block(network, block0.clone(), &Default::default(), None).unwrap();

    let block1 = block0
        .make_fake_child()
        .set_commitment(finalized_tree.hash().into());
    let block1_tree = make_tree(block1.clone(), &finalized_tree)?;

    let long_chain_block1 = block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(block1_tree.hash().into());
    let long_chain_block1_tree = make_tree(long_chain_block1.clone(), &block1_tree)?;
    let long_chain_block2 = long_chain_block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(long_chain_block1_tree.hash().into());

    let short_chain_block = block1
        .make_fake_child()
        .set_work(3)
        .set_commitment(block1_tree.hash().into());

    let mut state = NonFinalizedState::default();
    state.commit_new_chain(block1.prepare(), finalized_tree.clone())?;
    state.commit_block(long_chain_block1.prepare(), &finalized_tree)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_tree)?;
    state.commit_block(short_chain_block.prepare(), &finalized_tree)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(2, state.best_chain_len());

    Ok(())
}

#[test]
fn longer_chain_with_more_work_wins() -> Result<()> {
    zebra_test::init();

    longer_chain_with_more_work_wins_for_network(Network::Mainnet)?;
    longer_chain_with_more_work_wins_for_network(Network::Testnet)?;

    Ok(())
}

fn longer_chain_with_more_work_wins_for_network(network: Network) -> Result<()> {
    let block0: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };
    let finalized_tree =
        HistoryTree::new_from_block(network, block0.clone(), &Default::default(), None).unwrap();

    let block1 = block0
        .make_fake_child()
        .set_commitment(finalized_tree.hash().into());
    let block1_tree = make_tree(block1.clone(), &finalized_tree)?;

    let long_chain_block1 = block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(block1_tree.hash().into());
    let long_chain_block1_tree = make_tree(long_chain_block1.clone(), &block1_tree)?;
    let long_chain_block2 = long_chain_block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(long_chain_block1_tree.hash().into());
    let long_chain_block2_tree = make_tree(long_chain_block2.clone(), &long_chain_block1_tree)?;
    let long_chain_block3 = long_chain_block2
        .make_fake_child()
        .set_work(1)
        .set_commitment(long_chain_block2_tree.hash().into());
    let long_chain_block3_tree = make_tree(long_chain_block3.clone(), &long_chain_block2_tree)?;
    let long_chain_block4 = long_chain_block3
        .make_fake_child()
        .set_work(1)
        .set_commitment(long_chain_block3_tree.hash().into());

    let short_chain_block = block1
        .make_fake_child()
        .set_work(3)
        .set_commitment(block1_tree.hash().into());

    let mut state = NonFinalizedState::default();
    state.commit_new_chain(block1.prepare(), finalized_tree.clone())?;
    state.commit_block(long_chain_block1.prepare(), &finalized_tree)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_tree)?;
    state.commit_block(long_chain_block3.prepare(), &finalized_tree)?;
    state.commit_block(long_chain_block4.prepare(), &finalized_tree)?;
    state.commit_block(short_chain_block.prepare(), &finalized_tree)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(5, state.best_chain_len());

    Ok(())
}

#[test]
fn equal_length_goes_to_more_work() -> Result<()> {
    zebra_test::init();

    equal_length_goes_to_more_work_for_network(Network::Mainnet)?;
    equal_length_goes_to_more_work_for_network(Network::Testnet)?;

    Ok(())
}
fn equal_length_goes_to_more_work_for_network(network: Network) -> Result<()> {
    let block0: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };
    let finalized_tree =
        HistoryTree::new_from_block(network, block0.clone(), &Default::default(), None).unwrap();

    let block1 = block0
        .make_fake_child()
        .set_commitment(finalized_tree.hash().into());
    let block1_tree = make_tree(block1.clone(), &finalized_tree)?;

    let less_work_child = block1
        .make_fake_child()
        .set_work(1)
        .set_commitment(block1_tree.hash().into());
    let more_work_child = block1
        .make_fake_child()
        .set_work(3)
        .set_commitment(block1_tree.hash().into());
    let expected_hash = more_work_child.hash();

    let mut state = NonFinalizedState::default();
    state.commit_new_chain(block1.prepare(), finalized_tree.clone())?;
    state.commit_block(less_work_child.prepare(), &finalized_tree)?;
    state.commit_block(more_work_child.prepare(), &finalized_tree)?;
    assert_eq!(2, state.chain_set.len());

    let tip_hash = state.best_tip().unwrap().1;
    assert_eq!(expected_hash, tip_hash);

    Ok(())
}
