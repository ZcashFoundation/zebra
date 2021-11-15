//! Tests for Sprout Note Commitment Trees.

use std::sync::Arc;

use color_eyre::eyre;
use eyre::Result;

use hex::FromHex;
use zebra_test::vectors::{MAINNET_BLOCKS, MAINNET_FINAL_SPROUT_ROOTS};

use crate::block::Block;
use crate::parameters::{Network, NetworkUpgrade};
use crate::serialization::ZcashDeserializeInto;
use crate::sprout::commitment::NoteCommitment;
use crate::sprout::tests::test_vectors;
use crate::sprout::tree;

/// Tests if empty roots are generated correctly.
#[test]
fn empty_roots() {
    zebra_test::init();

    for i in 0..tree::EMPTY_ROOTS.len() {
        assert_eq!(
            hex::encode(tree::EMPTY_ROOTS[i]),
            // The test vector is in reversed order.
            test_vectors::HEX_EMPTY_ROOTS[tree::MERKLE_DEPTH - i]
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
    zebra_test::init();

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

    // TODO Get final Sprout roots for Testnet and uncomment the line below.

    // incremental_roots_with_blocks_for_network(Network::Testnet)?;

    Ok(())
}

fn incremental_roots_with_blocks_for_network(network: Network) -> Result<()> {
    let (blocks, sprout_roots) = match network {
        Network::Mainnet => (&*MAINNET_BLOCKS, &*MAINNET_FINAL_SPROUT_ROOTS),

        // TODO Use `TESTNET_BLOCKS` and `TESTNET_FINAL_SPROUT_ROOTS` once they are
        // available.
        Network::Testnet => (&*MAINNET_BLOCKS, &*MAINNET_FINAL_SPROUT_ROOTS),
    };

    // We pick the initial blocks of the Overwinter network upgrade. This is a
    // reasonable choice since Overwinter still uses the Sprout shielded pool,
    // and the block heights have a significant meaning in comparison to a
    // random Sprout block.
    let height = NetworkUpgrade::Overwinter
        .activation_height(network)
        .unwrap()
        .0;

    // Build an empty note commitment tree
    let mut note_commitment_tree = tree::NoteCommitmentTree::default();

    // Load the Overwinter activation block.
    let overwinter_activation_block = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Add note commitments from the Overwinter activation block to the tree.
    for transaction in overwinter_activation_block.transactions.iter() {
        for sprout_note_commitment in transaction.sprout_note_commitments() {
            note_commitment_tree
                .append(*sprout_note_commitment)
                .expect("we should not be able to fill up the tree");
        }
    }

    // Check if the root of the tree of the activation block is correct.
    let overwinter_activation_block_root =
        tree::Root(**sprout_roots.get(&height).expect("test vector exists"));
    assert_eq!(
        overwinter_activation_block_root,
        note_commitment_tree.root()
    );

    // Load the block immediately after Overwinter activation (activation + 1).
    let block_after_overwinter_activation = Arc::new(
        blocks
            .get(&(height + 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    let block_after_overwinter_activation_root =
        tree::Root(**sprout_roots.get(&(height + 1)).expect("test vector exists"));

    // Add note commitments from the block after Overwinter activation to the tree.
    let mut appended_count = 0;
    for transaction in block_after_overwinter_activation.transactions.iter() {
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
    // In the test vectors this applies only for the block 1 in mainnet,
    // so we make this explicit in this assert.
    if network == Network::Mainnet {
        assert!(appended_count > 1);
    }

    // Check if root of the second block is correct
    assert_eq!(
        block_after_overwinter_activation_root,
        note_commitment_tree.root()
    );

    Ok(())
}
