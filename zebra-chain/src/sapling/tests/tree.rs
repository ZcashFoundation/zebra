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

    // Load Block 0 (activation block of the given network upgrade)
    let block0 = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );
    // Build initial MMR tree with only Block 0
    let sapling_root0 =
        sapling::tree::Root(**sapling_roots.get(&height).expect("test vector exists"));

    let mut tree = sapling::tree::NoteCommitmentTree::default();

    // Add note commitments in Block 0 to the tree
    for transaction in block0.transactions.iter() {
        for sapling_note_commitment in transaction.sapling_note_commitments() {
            tree.append(*sapling_note_commitment)
                .expect("test vector is correct");
        }
    }

    // Check if root is correct
    assert_eq!(sapling_root0, tree.root());

    // Load Block 1 (activation + 1)
    let block1 = Arc::new(
        blocks
            .get(&(height + 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );
    let sapling_root1 = sapling::tree::Root(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    );

    // Add note commitments in Block 1 to the tree
    for transaction in block1.transactions.iter() {
        for sapling_note_commitment in transaction.sapling_note_commitments() {
            tree.append(*sapling_note_commitment)
                .expect("test vector is correct");
        }
    }

    // Check if root is correct
    assert_eq!(sapling_root1, tree.root());

    Ok(())
}
