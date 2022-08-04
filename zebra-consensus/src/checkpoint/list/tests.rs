//! Tests for CheckpointList

use super::*;

use std::sync::Arc;

use zebra_chain::parameters::{Network, Network::*};
use zebra_chain::{
    block::{self, Block},
    serialization::ZcashDeserialize,
};

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
fn checkpoint_list_bad_height_fail() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

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

    let _ = CheckpointList::new(Mainnet);
    let _ = CheckpointList::new(Testnet);

    Ok(())
}

#[test]
fn checkpoint_list_hard_coded_mandatory_mainnet() -> Result<(), BoxError> {
    checkpoint_list_hard_coded_mandatory(Mainnet)
}

#[test]
fn checkpoint_list_hard_coded_mandatory_testnet() -> Result<(), BoxError> {
    checkpoint_list_hard_coded_mandatory(Testnet)
}

/// Check that the hard-coded lists cover the mandatory checkpoint
fn checkpoint_list_hard_coded_mandatory(network: Network) -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let mandatory_checkpoint = network.mandatory_checkpoint_height();

    let list = CheckpointList::new(network);

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
    checkpoint_list_hard_coded_max_gap(Testnet)
}

/// Check that the hard-coded checkpoints are within `MAX_CHECKPOINT_HEIGHT_GAP`.
///
/// We can't test the block byte limit, because we don't have access to the entire
/// blockchain in the tests. But that's ok, because the byte limit only impacts
/// performance.
fn checkpoint_list_hard_coded_max_gap(network: Network) -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let list = CheckpointList::new(network);
    let mut heights = list.0.keys();

    // Check that we start at the genesis height
    let mut previous_height = block::Height(0);
    assert_eq!(heights.next(), Some(&previous_height));

    for height in heights {
        let height_limit = (previous_height + (crate::MAX_CHECKPOINT_HEIGHT_GAP as i32)).unwrap();
        assert!(
            height <= &height_limit,
            "Checkpoint gaps must be within MAX_CHECKPOINT_HEIGHT_GAP"
        );
        previous_height = *height;
    }

    Ok(())
}
