use std::sync::Arc;

use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::{Block, Height},
    orchard,
    parameters::Network,
    sapling,
    sapling::tree::legacy::{LegacyFrontier, LegacyLeaf, LegacyNonEmptyFrontier, LegacyPosition},
    serialization::ZcashDeserializeInto,
    subtree::{NoteCommitmentSubtree, TRACKED_SUBTREE_HEIGHT},
};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    request::{FinalizedBlock, Treestate},
    service::finalized_state::{DiskWriteBatch, WriteDisk, ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE},
    CheckpointVerifiedBlock, Config, MIN_PRUNING_RETENTION,
};

use super::{
    subtree_format_calculation_pre_checks, AFTER_MISSING_ORCHARD_SUBTREE,
    AFTER_MISSING_SAPLING_SUBTREE,
};

fn populated(config: &Config) -> ZebraDb {
    let mut db = ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Network::Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    db.update_format_version_on_disk(&state_database_format_version_in_code())
        .unwrap();

    for (_, bytes) in zebra_test::vectors::MAINNET_BLOCKS.range(0..=3) {
        let block = bytes.zcash_deserialize_into::<Arc<Block>>().unwrap();
        let finalized = FinalizedBlock::from_checkpoint_verified(
            CheckpointVerifiedBlock::from(block),
            Treestate::default(),
            DeferredPoolBalanceChange::zero(),
        );
        db.write_block(
            finalized,
            None,
            &Network::Mainnet,
            "subtree completion test",
            None,
            None,
        )
        .unwrap();
    }
    assert_eq!(db.finalized_tip_height(), Some(Height(3)));
    db
}

/// Finalization can complete the first subtree between its initial lookup and the tip-tree read.
#[test]
fn subtree_checks_allow_completion_after_missing_subtree_lookup() {
    use hex_literal::hex;

    let _init_guard = zebra_test::init();
    let config = Config {
        prune_blocks_older_than: Some(MIN_PRUNING_RETENTION),
        ..Config::ephemeral()
    };
    let db = populated(&config);
    db.mark_ready_for_pruning();
    assert!(db.sapling_subtree_by_index(0).is_none());
    assert!(db.orchard_subtree_by_index(0).is_none());
    assert!(db.sapling_tree_for_tip().subtree_index().is_none());
    assert!(db.orchard_tree_for_tip().subtree_index().is_none());

    let sapling = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(558_822),
        root: sapling_crypto::Node::from_bytes(hex!(
            "754bb593ea42d231a7ddf367640f09bbf59dc00f2c1d2003cc340e0c016b5b13"
        ))
        .unwrap(),
    };
    let orchard: NoteCommitmentSubtree<orchard::tree::Node> = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(1_707_429),
        root: hex!("d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b")
            .as_slice()
            .try_into()
            .unwrap(),
    };
    // Both tree types use this legacy encoding. One completed subtree followed by a note
    // needs only the known subtree root as its ommer, not all of the preceding commitments.
    let tree_bytes = |root| {
        let frontier = LegacyFrontier::<[u8; 32], 32> {
            frontier: Some(LegacyNonEmptyFrontier {
                position: LegacyPosition(1 << TRACKED_SUBTREE_HEIGHT),
                leaf: LegacyLeaf::Left([0; 32]),
                ommers: vec![root],
            }),
        };
        bincode::serialize(&(frontier, None::<[u8; 32]>)).unwrap()
    };
    let sapling_tree: sapling::tree::NoteCommitmentTree =
        bincode::deserialize(&tree_bytes(sapling.root.to_bytes())).unwrap();
    let orchard_tree: orchard::tree::NoteCommitmentTree =
        bincode::deserialize(&tree_bytes(orchard.root.to_repr())).unwrap();
    assert_eq!(sapling_tree.subtree_index(), Some(1.into()));
    assert_eq!(orchard_tree.subtree_index(), Some(1.into()));

    let writer = db.clone();
    AFTER_MISSING_SAPLING_SUBTREE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut batch = DiskWriteBatch::new();
            batch.create_sapling_tree(&writer, &sapling.end_height, &sapling_tree);
            batch.insert_sapling_subtree(&writer, &sapling);
            batch.zs_insert(
                &writer.cf_handle("hash_by_height").unwrap(),
                sapling.end_height,
                writer.hash(Height(3)).unwrap(),
            );
            writer.write_batch(batch).unwrap();
        }));
    });
    let writer = db.clone();
    AFTER_MISSING_ORCHARD_SUBTREE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut batch = DiskWriteBatch::new();
            batch.create_orchard_tree(&writer, &orchard.end_height, &orchard_tree);
            batch.insert_orchard_subtree(&writer, &orchard);
            batch.zs_insert(
                &writer.cf_handle("hash_by_height").unwrap(),
                orchard.end_height,
                writer.hash(Height(3)).unwrap(),
            );
            writer.write_batch(batch).unwrap();
        }));
    });

    let result = subtree_format_calculation_pre_checks(&db);
    assert!(AFTER_MISSING_SAPLING_SUBTREE.with(|hook| hook.borrow_mut().take().is_none()));
    assert!(AFTER_MISSING_ORCHARD_SUBTREE.with(|hook| hook.borrow_mut().take().is_none()));
    assert_eq!(result, Ok(()));
}
