use std::sync::Arc;

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};
use zebra_test::prelude::*;

use crate::{
    service::{
        finalized_state::FinalizedState,
        non_finalized_state::{Chain, NonFinalizedState},
    },
    tests::{FakeChainHelper, Prepare},
    Config,
};

use self::assert_eq;

#[test]
fn construct_empty() {
    zebra_test::init();
    let _chain = Chain::new(Default::default(), Default::default(), Default::default());
}

#[test]
fn construct_single() -> Result<()> {
    zebra_test::init();
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;

    let mut chain = Chain::new(Default::default(), Default::default(), Default::default());
    chain = chain.push(block.prepare())?;

    assert_eq!(1, chain.blocks.len());

    Ok(())
}

#[test]
fn construct_many() -> Result<()> {
    zebra_test::init();

    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let mut blocks = vec![];

    while blocks.len() < 100 {
        let next_block = block.make_fake_child();
        blocks.push(block);
        block = next_block;
    }

    let mut chain = Chain::new(Default::default(), Default::default(), Default::default());

    for block in blocks {
        chain = chain.push(block.prepare())?;
    }

    assert_eq!(100, chain.blocks.len());

    Ok(())
}

#[test]
fn ord_matches_work() -> Result<()> {
    zebra_test::init();
    let less_block = zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .set_work(1);
    let more_block = less_block.clone().set_work(10);

    let mut lesser_chain = Chain::new(Default::default(), Default::default(), Default::default());
    lesser_chain = lesser_chain.push(less_block.prepare())?;

    let mut bigger_chain = Chain::new(Default::default(), Default::default(), Default::default());
    bigger_chain = bigger_chain.push(more_block.prepare())?;

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

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let expected_hash = block2.hash();

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), network);

    state.commit_new_chain(block2.prepare(), &finalized_state)?;
    state.commit_new_chain(child.prepare(), &finalized_state)?;

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
    let block1: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), network);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(child.prepare(), &finalized_state)?;

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
    let block1: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };

    let block2 = block1.make_fake_child().set_work(10);
    let child1 = block1.make_fake_child().set_work(1);
    let child2 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), network);

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
    zebra_test::init();

    shorter_chain_can_be_best_chain_for_network(Network::Mainnet)?;
    shorter_chain_can_be_best_chain_for_network(Network::Testnet)?;

    Ok(())
}

fn shorter_chain_can_be_best_chain_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), network);

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
    zebra_test::init();

    longer_chain_with_more_work_wins_for_network(Network::Mainnet)?;
    longer_chain_with_more_work_wins_for_network(Network::Testnet)?;

    Ok(())
}

fn longer_chain_with_more_work_wins_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);
    let long_chain_block3 = long_chain_block2.make_fake_child().set_work(1);
    let long_chain_block4 = long_chain_block3.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), network);

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
    zebra_test::init();

    equal_length_goes_to_more_work_for_network(Network::Mainnet)?;
    equal_length_goes_to_more_work_for_network(Network::Testnet)?;

    Ok(())
}
fn equal_length_goes_to_more_work_for_network(network: Network) -> Result<()> {
    let block1: Arc<Block> = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_1326100_BYTES.zcash_deserialize_into()?
        }
    };

    let less_work_child = block1.make_fake_child().set_work(1);
    let more_work_child = block1.make_fake_child().set_work(3);
    let expected_hash = more_work_child.hash();

    let mut state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(&Config::ephemeral(), network);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(less_work_child.prepare(), &finalized_state)?;
    state.commit_block(more_work_child.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    let tip_hash = state.best_tip().unwrap().1;
    assert_eq!(expected_hash, tip_hash);

    Ok(())
}
