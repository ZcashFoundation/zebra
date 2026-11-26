//! Secondary-database commitment-tree consistency tests.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    orchard,
    parameters::Network::Mainnet,
    sapling,
    serialization::ZcashDeserializeInto,
};
use zebra_test::vectors::MAINNET_BLOCKS;

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    service::finalized_state::{
        disk_db::{DiskWriteBatch, WriteDisk as _},
        ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    Config, TransactionLocation,
};

fn open_mainnet_test_db(config: &Config, read_only: bool) -> ZebraDb {
    ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        read_only,
    )
    .unwrap()
}

fn find_mainnet_block(predicate: impl Fn(&Block) -> bool) -> Arc<Block> {
    MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .find(|block| predicate(block))
        .expect("mainnet test vectors contain a matching block")
}

fn write_block_columns(db: &ZebraDb, block: &Block) {
    let height = block.coinbase_height().unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.zs_insert(
        &db.cf_handle("hash_by_height").unwrap(),
        height,
        block.hash(),
    );
    batch.zs_insert(
        &db.cf_handle("height_by_hash").unwrap(),
        block.hash(),
        height,
    );
    batch.zs_insert(
        &db.cf_handle("block_header_by_height").unwrap(),
        height,
        &block.header,
    );
    for (index, tx) in block.transactions.iter().enumerate() {
        let location = TransactionLocation::from_usize(height, index);
        batch.zs_insert(&db.cf_handle("tx_by_loc").unwrap(), location, tx);
        batch.zs_insert(
            &db.cf_handle("hash_by_tx_loc").unwrap(),
            location,
            tx.hash(),
        );
        batch.zs_insert(
            &db.cf_handle("tx_loc_by_hash").unwrap(),
            tx.hash(),
            location,
        );
    }
    db.write_batch(batch).unwrap();
}

fn seed_empty_trees(db: &ZebraDb) {
    let mut seed_batch = DiskWriteBatch::new();
    seed_batch.create_sapling_tree(
        db,
        &Height(0),
        &sapling::tree::NoteCommitmentTree::default(),
    );
    seed_batch.create_orchard_tree(
        db,
        &Height(0),
        &orchard::tree::NoteCommitmentTree::default(),
    );
    db.write_batch(seed_batch).unwrap();
}

fn build_sapling_tree(block: &Block) -> sapling::tree::NoteCommitmentTree {
    let mut tree = sapling::tree::NoteCommitmentTree::default();
    for cm in block.sapling_note_commitments() {
        tree.append(cm).unwrap();
    }
    tree
}

fn build_orchard_tree(block: &Block) -> orchard::tree::NoteCommitmentTree {
    let mut tree = orchard::tree::NoteCommitmentTree::default();
    for cm in block.orchard_note_commitments() {
        tree.append(cm).unwrap();
    }
    tree
}

#[test]
fn secondary_tree_reads_do_not_expose_stale_trees_during_catch_up() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let primary = open_mainnet_test_db(&config, false);
    let sapling_block =
        find_mainnet_block(|block| block.sapling_note_commitments().next().is_some());
    let orchard_block =
        find_mainnet_block(|block| block.orchard_note_commitments().next().is_some());

    // Seed older empty trees at Height(0), then expose both blocks' header/tx columns without
    // writing their updated commitment trees yet.
    seed_empty_trees(&primary);
    write_block_columns(&primary, &sapling_block);
    write_block_columns(&primary, &orchard_block);
    primary.db.flush().unwrap();

    let secondary = open_mainnet_test_db(&config, true);
    let sapling_height = sapling_block.coinbase_height().unwrap();
    let orchard_height = orchard_block.coinbase_height().unwrap();

    // Missing new-block tree entries must return None on secondary, not the stale Height(0) tree.
    assert!(secondary
        .sapling_tree_by_hash_or_height(sapling_block.hash().into())
        .is_none());
    assert!(secondary
        .sapling_tree_by_hash_or_height(sapling_height.into())
        .is_none());
    assert!(secondary
        .orchard_tree_by_hash_or_height(orchard_block.hash().into())
        .is_none());
    assert!(secondary
        .orchard_tree_by_hash_or_height(orchard_height.into())
        .is_none());

    // Once the updated trees are written and caught up, secondary serves them normally.
    let sapling_tree = build_sapling_tree(&sapling_block);
    let orchard_tree = build_orchard_tree(&orchard_block);
    let mut tree_batch = DiskWriteBatch::new();
    tree_batch.create_sapling_tree(&primary, &sapling_height, &sapling_tree);
    tree_batch.create_orchard_tree(&primary, &orchard_height, &orchard_tree);
    primary.write_batch(tree_batch).unwrap();
    primary.db.flush().unwrap();
    secondary.try_catch_up_with_primary().unwrap();

    assert_eq!(
        secondary.sapling_tree_by_hash_or_height(sapling_block.hash().into()),
        Some(Arc::new(sapling_tree))
    );
    assert_eq!(
        secondary.orchard_tree_by_hash_or_height(orchard_block.hash().into()),
        Some(Arc::new(orchard_tree))
    );
}
