//! Tests for reading the embedded trees from a state database.

use std::sync::Arc;

use tower::ServiceExt;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    sapling,
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
};
use zebra_state::{DiskWriteBatch, EmbeddedTrees, Request};
use zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS;

use super::embedded_trees;

#[tokio::test]
async fn embedded_trees_round_trip_from_a_state() {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    let (state, read_state, _, _) = zebra_state::init_test_services(&network).await;

    for block in CONTINUOUS_TESTNET_BLOCKS.values() {
        let block: Arc<Block> = block
            .zcash_deserialize_into()
            .expect("test vector blocks are valid");
        state
            .clone()
            .oneshot(Request::CommitCheckpointVerifiedBlock(block.into()))
            .await
            .expect("test vector blocks are a valid chain");
    }

    let db = read_state.db();
    let tip = db
        .finalized_tip_height()
        .expect("the blocks were just committed");
    let below_tip = Height(tip.0 - 1);

    // The test vectors have no shielded outputs, so give the tip a non-empty Sapling tree.
    let mut sapling = sapling::tree::NoteCommitmentTree::default();
    for _ in 0..3 {
        let cm_u = sapling_crypto::note::ExtractedNoteCommitment::from_bytes(&[0; 32])
            .expect("zero is a valid commitment");
        sapling.append(cm_u).expect("the tree has room");
    }
    let mut batch = DiskWriteBatch::new();
    batch.create_sapling_tree(db, &tip, &sapling);
    db.write_batch(batch)
        .expect("the test database is writable");

    let trees = embedded_trees(db, tip).expect("the tip has trees");
    assert_eq!(trees.height, tip);
    assert_eq!(trees.sapling.count(), 3);
    assert_eq!(trees.sapling.root(), sapling.root());
    assert_eq!(
        EmbeddedTrees::from_bytes(&trees.to_bytes()).expect("serialized trees decode"),
        trees,
    );

    assert!(
        embedded_trees(db, Height(tip.0 + 1)).is_err(),
        "above the finalized tip"
    );

    // A subtree that the tip's 3-leaf tree didn't complete.
    let root = sapling_crypto::Node::from_bytes([0; 32]).expect("zero is a valid node");
    let mut batch = DiskWriteBatch::new();
    batch.insert_sapling_subtree(db, &NoteCommitmentSubtree::new(0u16, tip, root));
    db.write_batch(batch)
        .expect("the test database is writable");

    assert!(embedded_trees(db, tip).is_err(), "a subtree too many");
    assert!(
        embedded_trees(db, below_tip)
            .expect("the subtree ends above this height")
            .sapling_subtrees
            .is_empty(),
        "only subtrees up to the height"
    );
}
