//! Tests for CheckpointList

use std::sync::Arc;

use num_integer::div_ceil;

use zebra_chain::{
    block::{Block, HeightDiff, MAX_BLOCK_BYTES},
    parameters::Network::*,
    serialization::ZcashDeserialize,
};
use zebra_node_services::constants::{MAX_CHECKPOINT_BYTE_COUNT, MAX_CHECKPOINT_HEIGHT_GAP};

use super::*;

/// Make a checkpoint list containing only the genesis block
#[test]
fn checkpoint_list_genesis() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

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
fn checkpoint_list_multiple() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

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
fn checkpoint_list_empty_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let _ = CheckpointList::from_list(Vec::new()).expect_err("empty checkpoint lists should fail");

    Ok(())
}

/// Make sure a checkpoint list that doesn't contain the genesis block fails
#[test]
fn checkpoint_list_no_genesis_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

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
fn checkpoint_list_null_hash_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let checkpoint_data = [(block::Height(0), block::Hash([0; 32]))];

    // Make a checkpoint list containing the non-genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list)
        .expect_err("a checkpoint list with a null block hash should fail");

    Ok(())
}

/// Make sure a checkpoint list that contains an invalid block height fails
#[test]
fn checkpoint_list_bad_height_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let checkpoint_data = [(
        block::Height(block::Height::MAX.0 + 1),
        block::Hash([1; 32]),
    )];

    // Make a checkpoint list containing the non-genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list).expect_err(
        "a checkpoint list with an invalid block height (block::Height::MAX + 1) should fail",
    );

    let checkpoint_data = [(block::Height(u32::MAX), block::Hash([1; 32]))];

    // Make a checkpoint list containing the non-genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let _ = CheckpointList::from_list(checkpoint_list)
        .expect_err("a checkpoint list with an invalid block height (u32::MAX) should fail");

    Ok(())
}

/// Make sure that a checkpoint list containing duplicate blocks fails
#[test]
fn checkpoint_list_duplicate_blocks_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

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
fn checkpoint_list_duplicate_heights_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    // Parse the genesis block
    let mut checkpoint_data = Vec::new();
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash = block.hash();
    checkpoint_data.push((
        block.coinbase_height().expect("test block has height"),
        hash,
    ));

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
fn checkpoint_list_duplicate_hashes_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    // Parse the genesis block
    let mut checkpoint_data = Vec::new();
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash = block.hash();
    checkpoint_data.push((
        block.coinbase_height().expect("test block has height"),
        hash,
    ));

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
fn checkpoint_list_load_hard_coded() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let _: CheckpointList = MAINNET_CHECKPOINTS
        .parse()
        .expect("hard-coded Mainnet checkpoint list should parse");
    let _: CheckpointList = TESTNET_CHECKPOINTS
        .parse()
        .expect("hard-coded Testnet checkpoint list should parse");

    let _ = Mainnet.checkpoint_list();
    let _ = Network::new_default_testnet().checkpoint_list();
    let _ = Network::new_regtest(Default::default()).checkpoint_list();

    Ok(())
}

#[test]
fn checkpoint_list_hard_coded_mandatory_mainnet() -> Result<(), BoxError> {
    checkpoint_list_hard_coded_mandatory(Mainnet)
}

#[test]
fn checkpoint_list_hard_coded_mandatory_testnet() -> Result<(), BoxError> {
    checkpoint_list_hard_coded_mandatory(Network::new_default_testnet())
}

/// Check that the hard-coded lists cover the mandatory checkpoint
fn checkpoint_list_hard_coded_mandatory(network: Network) -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let mandatory_checkpoint = network.mandatory_checkpoint_height();

    let list = network.checkpoint_list();

    assert!(
        list.max_height() >= mandatory_checkpoint,
        "Mandatory checkpoint block must be verified by checkpoints"
    );

    Ok(())
}

#[test]
fn checkpoint_list_hard_coded_max_gap_mainnet() -> Result<(), BoxError> {
    checkpoint_list_hard_coded_max_gap(Mainnet)
}

#[test]
fn checkpoint_list_hard_coded_max_gap_testnet() -> Result<(), BoxError> {
    checkpoint_list_hard_coded_max_gap(Network::new_default_testnet())
}

/// Check that the hard-coded checkpoints are within [`MAX_CHECKPOINT_HEIGHT_GAP`],
/// and a calculated minimum number of blocks. This also checks the heights are in order.
///
/// We can't test [`MAX_CHECKPOINT_BYTE_COUNT`] directly, because we don't have access to a large
/// enough blockchain in the tests. Instead, we check the number of maximum-size blocks in a
/// checkpoint. (This is ok, because the byte count only impacts performance.)
fn checkpoint_list_hard_coded_max_gap(network: Network) -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let max_checkpoint_height_gap =
        HeightDiff::try_from(MAX_CHECKPOINT_HEIGHT_GAP).expect("constant fits in HeightDiff");
    let min_checkpoint_height_gap =
        HeightDiff::try_from(div_ceil(MAX_CHECKPOINT_BYTE_COUNT, MAX_BLOCK_BYTES))
            .expect("constant fits in HeightDiff");

    let list = network.checkpoint_list();
    let mut heights = list.0.keys();

    // Check that we start at the genesis height
    let mut previous_height = block::Height(0);
    assert_eq!(heights.next(), Some(&previous_height));

    for height in heights {
        let height_upper_limit = (previous_height + max_checkpoint_height_gap)
            .expect("checkpoint heights are valid blockchain heights");

        let height_lower_limit = (previous_height + min_checkpoint_height_gap)
            .expect("checkpoint heights are valid blockchain heights");

        assert!(
            height <= &height_upper_limit,
            "Checkpoint gaps must be MAX_CHECKPOINT_HEIGHT_GAP or less \
             actually: {height:?} - {previous_height:?} = {} \
             should be: less than or equal to {max_checkpoint_height_gap}",
            *height - previous_height,
        );
        assert!(
            height >= &height_lower_limit,
            "Checkpoint gaps must be ceil(MAX_CHECKPOINT_BYTE_COUNT/MAX_BLOCK_BYTES) or greater \
             actually: {height:?} - {previous_height:?} = {} \
             should be: greater than or equal to {min_checkpoint_height_gap}",
            *height - previous_height,
        );

        previous_height = *height;
    }

    Ok(())
}
