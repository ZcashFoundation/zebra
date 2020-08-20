//! Tests for CheckpointList

use super::*;

use std::ops::Bound::*;
use std::sync::Arc;

use zebra_chain::parameters::{Network, Network::*, NetworkUpgrade, NetworkUpgrade::*};
use zebra_chain::{
    block::{self, Block},
    serialization::ZcashDeserialize,
};

/// Make a checkpoint list containing only the genesis block
#[test]
fn checkpoint_list_genesis() -> Result<(), Error> {
    zebra_test::init();

    // Parse the genesis block
    let mut checkpoint_data = Vec::new();
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash = block.hash();
    checkpoint_data.push((
        block.coinbase_height().expect("test block has height"),
        hash,
    ));

    // Make a checkpoint list containing the genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list)?;

    Ok(())
}

/// Make a checkpoint list containing multiple blocks
#[test]
fn checkpoint_list_multiple() -> Result<(), Error> {
    zebra_test::init();

    // Parse all the blocks
    let mut checkpoint_data = Vec::new();
    for b in &[
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));
    }

    // Make a checkpoint list containing all the blocks
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list)?;

    Ok(())
}

/// Make sure that an empty checkpoint list fails
#[test]
fn checkpoint_list_empty_fail() -> Result<(), Error> {
    zebra_test::init();

    let _ = CheckpointList::from_list(Vec::new()).expect_err("empty checkpoint lists should fail");

    Ok(())
}

/// Make sure a checkpoint list that doesn't contain the genesis block fails
#[test]
fn checkpoint_list_no_genesis_fail() -> Result<(), Error> {
    zebra_test::init();

    // Parse a non-genesis block
    let mut checkpoint_data = Vec::new();
    let block = Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])?;
    let hash = block.hash();
    checkpoint_data.push((
        block.coinbase_height().expect("test block has height"),
        hash,
    ));

    // Make a checkpoint list containing the non-genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list)
        .expect_err("a checkpoint list with no genesis block should fail");

    Ok(())
}

/// Make sure a checkpoint list that contains a null hash fails
#[test]
fn checkpoint_list_null_hash_fail() -> Result<(), Error> {
    zebra_test::init();

    let checkpoint_data = vec![(block::Height(0), block::Hash([0; 32]))];

    // Make a checkpoint list containing the non-genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list)
        .expect_err("a checkpoint list with a null block hash should fail");

    Ok(())
}

/// Make sure a checkpoint list that contains an invalid block height fails
#[test]
fn checkpoint_list_bad_height_fail() -> Result<(), Error> {
    zebra_test::init();

    let checkpoint_data = vec![(
        block::Height(block::Height::MAX.0 + 1),
        block::Hash([1; 32]),
    )];

    // Make a checkpoint list containing the non-genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list).expect_err(
        "a checkpoint list with an invalid block height (block::Height::MAX + 1) should fail",
    );

    let checkpoint_data = vec![(block::Height(u32::MAX), block::Hash([1; 32]))];

    // Make a checkpoint list containing the non-genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list)
        .expect_err("a checkpoint list with an invalid block height (u32::MAX) should fail");

    Ok(())
}

/// Make sure that a checkpoint list containing duplicate blocks fails
#[test]
fn checkpoint_list_duplicate_blocks_fail() -> Result<(), Error> {
    zebra_test::init();

    // Parse some blocks twice
    let mut checkpoint_data = Vec::new();
    for b in &[
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));
    }

    // Make a checkpoint list containing some duplicate blocks
    let _ = CheckpointList::from_list(checkpoint_data)
        .expect_err("checkpoint lists with duplicate blocks should fail");

    Ok(())
}

/// Make sure that a checkpoint list containing duplicate heights
/// (with different hashes) fails
#[test]
fn checkpoint_list_duplicate_heights_fail() -> Result<(), Error> {
    zebra_test::init();

    // Parse the genesis block
    let mut checkpoint_data = Vec::new();
    for b in &[&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));
    }

    // Then add some fake entries with duplicate heights
    checkpoint_data.push((block::Height(1), block::Hash([0xaa; 32])));
    checkpoint_data.push((block::Height(1), block::Hash([0xbb; 32])));

    // Make a checkpoint list containing some duplicate blocks
    let _ = CheckpointList::from_list(checkpoint_data)
        .expect_err("checkpoint lists with duplicate heights should fail");

    Ok(())
}

/// Make sure that a checkpoint list containing duplicate hashes
/// (at different heights) fails
#[test]
fn checkpoint_list_duplicate_hashes_fail() -> Result<(), Error> {
    zebra_test::init();

    // Parse the genesis block
    let mut checkpoint_data = Vec::new();
    for b in &[&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));
    }

    // Then add some fake entries with duplicate hashes
    checkpoint_data.push((block::Height(1), block::Hash([0xcc; 32])));
    checkpoint_data.push((block::Height(2), block::Hash([0xcc; 32])));

    // Make a checkpoint list containing some duplicate blocks
    let _ = CheckpointList::from_list(checkpoint_data)
        .expect_err("checkpoint lists with duplicate hashes should fail");

    Ok(())
}

/// Parse and check the hard-coded Mainnet and Testnet lists
#[test]
fn checkpoint_list_load_hard_coded() -> Result<(), Error> {
    zebra_test::init();

    let _: CheckpointList = MAINNET_CHECKPOINTS
        .parse()
        .expect("hard-coded Mainnet checkpoint list should parse");
    let _: CheckpointList = TESTNET_CHECKPOINTS
        .parse()
        .expect("hard-coded Testnet checkpoint list should parse");

    let _ = CheckpointList::new(Mainnet);
    let _ = CheckpointList::new(Testnet);

    Ok(())
}

#[test]
fn checkpoint_list_hard_coded_sapling_mainnet() -> Result<(), Error> {
    checkpoint_list_hard_coded_sapling(Mainnet)
}

#[test]
fn checkpoint_list_hard_coded_sapling_testnet() -> Result<(), Error> {
    checkpoint_list_hard_coded_sapling(Testnet)
}

/// Check that the hard-coded lists cover the Sapling network upgrade
fn checkpoint_list_hard_coded_sapling(network: Network) -> Result<(), Error> {
    zebra_test::init();

    let sapling_activation = Sapling
        .activation_height(network)
        .expect("Unexpected network upgrade info: Sapling must have an activation height");

    let list = CheckpointList::new(network);

    assert!(
        list.max_height() >= sapling_activation,
        "Pre-Sapling blocks must be verified by checkpoints"
    );

    Ok(())
}

#[test]
fn checkpoint_list_up_to_mainnet() -> Result<(), Error> {
    checkpoint_list_up_to(Mainnet, Sapling)?;
    checkpoint_list_up_to(Mainnet, Blossom)?;
    checkpoint_list_up_to(Mainnet, Heartwood)?;
    checkpoint_list_up_to(Mainnet, Canopy)?;

    Ok(())
}

#[test]
fn checkpoint_list_up_to_testnet() -> Result<(), Error> {
    checkpoint_list_up_to(Testnet, Sapling)?;
    checkpoint_list_up_to(Testnet, Blossom)?;
    checkpoint_list_up_to(Testnet, Heartwood)?;
    checkpoint_list_up_to(Testnet, Canopy)?;

    Ok(())
}

/// Check that CheckpointList::new_up_to works
fn checkpoint_list_up_to(network: Network, limit: NetworkUpgrade) -> Result<(), Error> {
    zebra_test::init();

    let sapling_activation = Sapling
        .activation_height(network)
        .expect("Unexpected network upgrade info: Sapling must have an activation height");

    let limited_list = CheckpointList::new_up_to(network, limit);
    let full_list = CheckpointList::new(network);

    assert!(
        limited_list.max_height() >= sapling_activation,
        "Pre-Sapling blocks must be verified by checkpoints"
    );

    if let Some(limit_activation) = limit.activation_height(network) {
        if limit_activation <= full_list.max_height() {
            assert!(
                limited_list.max_height() >= limit_activation,
                "The 'limit' network upgrade must be verified by checkpoints"
            );

            let next_checkpoint_after_limit = limited_list
                .min_height_in_range((Included(limit_activation), Unbounded))
                .expect("There must be a checkpoint at or after the limit");

            assert_eq!(
                limited_list
                    .min_height_in_range((Excluded(next_checkpoint_after_limit), Unbounded)),
                None,
                "There must not be multiple checkpoints after the limit"
            );

            let next_activation = NetworkUpgrade::next(network, limit_activation)
                .map(|nu| nu.activation_height(network))
                .flatten();
            if let Some(next_activation) = next_activation {
                // We expect that checkpoints happen much more often than network upgrades
                assert!(
                    limited_list.max_height() < next_activation,
                    "The next network upgrade after 'limit' must not be verified by checkpoints"
                );
            }

            // We have an effective limit, so skip the "no limit" test
            return Ok(());
        }
    }

    // Either the activation height is unspecified, or it is above the maximum
    // checkpoint height (in the full checkpoint list)
    assert_eq!(
        limited_list.max_height(),
        full_list.max_height(),
        "Future network upgrades must not limit checkpoints"
    );

    Ok(())
}
