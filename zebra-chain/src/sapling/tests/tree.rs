use std::sync::Arc;

use color_eyre::eyre;
use eyre::Result;
use hex::FromHex;

use crate::block::Block;
use crate::parameters::NetworkUpgrade;
use crate::sapling::{self, tree::*};
use crate::serialization::ZcashDeserializeInto;
use crate::{parameters::Network, sapling::tests::test_vectors};

#[test]
fn empty_roots() {
    let _init_guard = zebra_test::init();

    for i in 0..EMPTY_ROOTS.len() {
        assert_eq!(
            hex::encode(EMPTY_ROOTS[i]),
            // The test vector is in reversed order.
            test_vectors::HEX_EMPTY_ROOTS[usize::from(MERKLE_DEPTH) - i]
        );
    }
}

#[test]
fn incremental_roots() {
    let _init_guard = zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = NoteCommitmentTree::default();

    for (i, cm_u) in test_vectors::COMMITMENTS.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_u).unwrap();

        let cm_u = sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&bytes).unwrap();

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
    for network in Network::iter() {
        incremental_roots_with_blocks_for_network(network)?;
    }
    Ok(())
}

fn incremental_roots_with_blocks_for_network(network: Network) -> Result<()> {
    let (blocks, sapling_roots) = network.block_sapling_roots_map();

    let height = NetworkUpgrade::Sapling
        .activation_height(&network)
        .unwrap()
        .0;

    // Build empty note commitment tree
    let mut tree = sapling::tree::NoteCommitmentTree::default();

    // Load Sapling activation block
    let sapling_activation_block = Arc::new(
        blocks
            .get(&height)
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    // Add note commitments from the Sapling activation block to the tree
    for transaction in sapling_activation_block.transactions.iter() {
        for sapling_note_commitment in transaction.sapling_note_commitments() {
            tree.append(*sapling_note_commitment)
                .expect("test vector is correct");
        }
    }

    // Check if root of the tree of the activation block is correct
    let sapling_activation_block_root =
        sapling::tree::Root::try_from(**sapling_roots.get(&height).expect("test vector exists"))?;
    assert_eq!(sapling_activation_block_root, tree.root());

    // Load the block immediately after Sapling activation (activation + 1)
    let block_after_sapling_activation = Arc::new(
        blocks
            .get(&(height + 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );
    let block_after_sapling_activation_root = sapling::tree::Root::try_from(
        **sapling_roots
            .get(&(height + 1))
            .expect("test vector exists"),
    )?;

    // Add note commitments from the block after Sapling activatoin to the tree
    let mut appended_count = 0;
    for transaction in block_after_sapling_activation.transactions.iter() {
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
    assert_eq!(block_after_sapling_activation_root, tree.root());

    Ok(())
}
