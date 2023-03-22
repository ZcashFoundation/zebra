//! Tests for Sprout Note Commitment Trees.

use std::sync::Arc;

use color_eyre::eyre;
use eyre::Result;
use hex::FromHex;

use zebra_test::vectors;

use crate::{
    block::Block,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    sprout::{commitment::NoteCommitment, tests::test_vectors, tree},
};

/// Tests if empty roots are generated correctly.
#[test]
fn empty_roots() {
    let _init_guard = zebra_test::init();

    for i in 0..tree::EMPTY_ROOTS.len() {
        assert_eq!(
            hex::encode(tree::EMPTY_ROOTS[i]),
            // The test vector is in reversed order.
            test_vectors::HEX_EMPTY_ROOTS[usize::from(tree::MERKLE_DEPTH) - i]
        );
    }
}

/// Tests if we have the right unused (empty) leaves.
#[test]
fn empty_leaf() {
    assert_eq!(
        tree::NoteCommitmentTree::uncommitted(),
        test_vectors::EMPTY_LEAF
    );
}

/// Tests if we can build the tree correctly.
#[test]
fn incremental_roots() {
    let _init_guard = zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = tree::NoteCommitmentTree::default();

    for (i, cm) in test_vectors::COMMITMENTS.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm).unwrap();
        let cm = NoteCommitment::from(bytes);

        // Test if we can append a new note commitment to the tree.
        let _ = incremental_tree.append(cm);
        let incremental_root_hash = incremental_tree.hash();
        assert_eq!(hex::encode(incremental_root_hash), test_vectors::ROOTS[i]);

        // The root hashes should match.
        let incremental_root = incremental_tree.root();
        assert_eq!(<[u8; 32]>::from(incremental_root), incremental_root_hash);

        // Test if the note commitments are counted correctly.
        assert_eq!(incremental_tree.count(), (i + 1) as u64);

        // Test if we can build the tree from a vector of note commitments
        // instead of appending only one note commitment to the tree.
        leaves.push(cm);
        let ad_hoc_tree = tree::NoteCommitmentTree::from(leaves.clone());
        let ad_hoc_root_hash = ad_hoc_tree.hash();
        assert_eq!(hex::encode(ad_hoc_root_hash), test_vectors::ROOTS[i]);

        // The root hashes should match.
        let ad_hoc_root = ad_hoc_tree.root();
        assert_eq!(<[u8; 32]>::from(ad_hoc_root), ad_hoc_root_hash);

        // Test if the note commitments are counted correctly.
        assert_eq!(ad_hoc_tree.count(), (i + 1) as u64);
    }
}

#[test]
fn incremental_roots_with_blocks() -> Result<()> {
    incremental_roots_with_blocks_for_network(Network::Mainnet)?;
    incremental_roots_with_blocks_for_network(Network::Testnet)?;

    Ok(())
}

fn incremental_roots_with_blocks_for_network(network: Network) -> Result<()> {
    // The mainnet block height at which the first JoinSplit occurred.
    const MAINNET_FIRST_JOINSPLIT_HEIGHT: u32 = 396;

    // The testnet block height at which the first JoinSplit occurred.
    const TESTNET_FIRST_JOINSPLIT_HEIGHT: u32 = 2259;

    // Load the test data.
    let (blocks, sprout_roots, next_height) = match network {
        Network::Mainnet => (
            &*vectors::MAINNET_BLOCKS,
            &*vectors::MAINNET_FINAL_SPROUT_ROOTS,
            MAINNET_FIRST_JOINSPLIT_HEIGHT,
        ),
        Network::Testnet => (
            &*vectors::TESTNET_BLOCKS,
            &*vectors::TESTNET_FINAL_SPROUT_ROOTS,
            TESTNET_FIRST_JOINSPLIT_HEIGHT,
        ),
    };

    // Load the Genesis height.
    let genesis_height = NetworkUpgrade::Genesis
        .activation_height(network)
        .unwrap()
        .0;

    // Load the Genesis block.
    let genesis_block = Arc::new(
        blocks
            .get(&genesis_height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Build an empty note commitment tree.
    let mut note_commitment_tree = tree::NoteCommitmentTree::default();

    // Add note commitments from Genesis to the tree.
    for transaction in genesis_block.transactions.iter() {
        for sprout_note_commitment in transaction.sprout_note_commitments() {
            note_commitment_tree
                .append(*sprout_note_commitment)
                .expect("we should not be able to fill up the tree");
        }
    }

    // Load the Genesis note commitment tree root.
    let genesis_anchor = tree::Root::from(
        **sprout_roots
            .get(&genesis_height)
            .expect("test vector exists"),
    );

    // Check if the root of the note commitment tree of Genesis is correct.
    assert_eq!(genesis_anchor, note_commitment_tree.root());

    // Load the first block after Genesis that contains a JoinSplit transaction
    // so that we can add new note commitments to the tree.
    let next_block = Arc::new(
        blocks
            .get(&(genesis_height + next_height))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Load the note commitment tree root of `next_block`.
    let next_block_anchor = tree::Root::from(
        **sprout_roots
            .get(&(genesis_height + next_height))
            .expect("test vector exists"),
    );

    // Add the note commitments from `next_block` to the tree.
    let mut appended_count = 0;
    for transaction in next_block.transactions.iter() {
        for sprout_note_commitment in transaction.sprout_note_commitments() {
            note_commitment_tree
                .append(*sprout_note_commitment)
                .expect("test vector is correct");
            appended_count += 1;
        }
    }

    // We also want to make sure that sprout_note_commitments() is returning
    // the commitments in the right order. But this will only be actually tested
    // if there are more than one note commitment in a block.
    assert!(appended_count > 1);

    // Check if the root of `next_block` is correct.
    assert_eq!(next_block_anchor, note_commitment_tree.root());

    Ok(())
}
