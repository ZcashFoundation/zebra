use std::sync::Arc;

use color_eyre::eyre;
use eyre::Result;
use hex::FromHex;

use crate::block::Block;
use crate::parameters::NetworkUpgrade;
use crate::sapling::{self, tree::*};
use crate::serialization::ZcashDeserializeInto;
use crate::{parameters::Network, sapling::tests::test_vectors};
use zebra_test::vectors::{
    MAINNET_BLOCKS, MAINNET_FINAL_SAPLING_ROOTS, TESTNET_BLOCKS, TESTNET_FINAL_SAPLING_ROOTS,
};

#[test]
fn empty_roots() {
    zebra_test::init();

    for i in 0..EMPTY_ROOTS.len() {
        assert_eq!(
            hex::encode(EMPTY_ROOTS[i]),
            // The test vector is in reversed order.
            test_vectors::HEX_EMPTY_ROOTS[MERKLE_DEPTH - i]
        );
    }
}

#[test]
fn incremental_roots() {
    zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = NoteCommitmentTree::default();

    for (i, cm_u) in test_vectors::COMMITMENTS.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_u).unwrap();

        let cm_u = jubjub::Fq::from_bytes(&bytes).unwrap();

        leaves.push(cm_u);

        let _ = incremental_tree.append(cm_u);

        assert_eq!(hex::encode(incremental_tree.hash()), test_vectors::ROOTS[i]);

        assert_eq!(
            hex::encode((NoteCommitmentTree::from(leaves.clone())).hash()),
            test_vectors::ROOTS[i]
        );
    }
}

#[test]
fn incremental_roots_with_blocks() -> Result<()> {
    incremental_roots_with_blocks_for_network(Network::Mainnet)?;
    incremental_roots_with_blocks_for_network(Network::Testnet)?;
    Ok(())
}

fn incremental_roots_with_blocks_for_network(network: Network) -> Result<()> {
    let (blocks, sapling_roots) = match network {
        Network::Mainnet => (&*MAINNET_BLOCKS, &*MAINNET_FINAL_SAPLING_ROOTS),
        Network::Testnet => (&*TESTNET_BLOCKS, &*TESTNET_FINAL_SAPLING_ROOTS),
    };
    let height = NetworkUpgrade::Sapling
        .activation_height(network)
        .unwrap()
        .0;

    // Build empty note commitment tree
    let mut tree = sapling::tree::NoteCommitmentTree::default();

    // Load first block (activation block of the given network upgrade)
    let first_block = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Add note commitments in the first block to the tree
    for transaction in first_block.transactions.iter() {
        for sapling_note_commitment in transaction.sapling_note_commitments() {
            tree.append(*sapling_note_commitment)
                .expect("test vector is correct");
        }
    }

    // Check if root of the first block is correct
    let first_sapling_root =
        sapling::tree::Root(**sapling_roots.get(&height).expect("test vector exists"));
    assert_eq!(first_sapling_root, tree.root());

    // Load second block (activation + 1)
    let second_block = Arc::new(
        blocks
            .get(&(height + 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );
    let second_sapling_root = sapling::tree::Root(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    );

    // Add note commitments in second block to the tree
    let mut appended_count = 0;
    for transaction in second_block.transactions.iter() {
        for sapling_note_commitment in transaction.sapling_note_commitments() {
            tree.append(*sapling_note_commitment)
                .expect("test vector is correct");
            appended_count += 1;
        }
    }
    // We also want to make sure that sapling_note_commitments() is returning
    // the commitments in the right order. But this will only be actually tested
    // if there are more than one note commitment in a block.
    // In the test vectors this applies only for the block 1 in mainnet,
    // so we make this explicit in this assert.
    if network == Network::Mainnet {
        assert!(appended_count > 1);
    }

    // Check if root of the second block is correct
    assert_eq!(second_sapling_root, tree.root());

    Ok(())
}
