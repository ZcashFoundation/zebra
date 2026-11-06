//! Tests for the `sync_meta_by_height` column family and its format upgrade.

use std::sync::Arc;

use zebra_chain::{
    block::{merkle::AuthDataRoot, Block, Height},
    parameters::Network,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

use crate::{
    service::finalized_state::{
        disk_format::{
            chain::SyncMetadata,
            upgrade::{add_sync_metadata, CancelFormatChange, DiskFormatUpgrade},
        },
        FinalizedState, ZebraDb,
    },
    CheckpointVerifiedBlock, Config,
};

/// Returns the network's contiguous test blocks from genesis.
fn test_blocks(network: &Network) -> Vec<Arc<Block>> {
    network
        .blockchain_map()
        .iter()
        .enumerate()
        .take_while(|(index, (height, _))| usize::try_from(**height) == Ok(*index))
        .map(|(_, (_, bytes))| {
            bytes
                .zcash_deserialize_into()
                .expect("test data deserializes")
        })
        .collect()
}

/// The note commitment and per-pool transaction counts of `block`, scanned
/// directly from its transactions, independently of the block-level helpers
/// [`SyncMetadata::for_block`] uses.
///
/// Returns `(notes, sapling_txs, orchard_txs, ironwood_txs)`, where `notes`
/// follows the spec: two per JoinSplit, one per Sapling output, one per
/// Orchard or Ironwood action.
fn shielded_scan(block: &Block) -> (usize, usize, usize, usize) {
    let mut notes = 0;
    let (mut sapling_txs, mut orchard_txs, mut ironwood_txs) = (0, 0, 0);

    for tx in &block.transactions {
        let sapling_outputs = tx.sapling_outputs().count();
        let orchard_actions = tx.orchard_actions().count();
        let ironwood_actions = tx.ironwood_actions().count();

        notes += 2 * tx.joinsplit_count() + sapling_outputs + orchard_actions + ironwood_actions;

        if tx.sapling_spends_count() > 0 || sapling_outputs > 0 {
            sapling_txs += 1;
        }
        if orchard_actions > 0 {
            orchard_txs += 1;
        }
        if ironwood_actions > 0 {
            ironwood_txs += 1;
        }
    }

    (notes, sapling_txs, orchard_txs, ironwood_txs)
}

/// Asserts that the note commitment and per-pool transaction counts in
/// `metadata` match a direct scan of `block`.
fn assert_shielded_counts_match_scan(metadata: &SyncMetadata, block: &Block, context: &str) {
    let (notes, sapling_txs, orchard_txs, ironwood_txs) = shielded_scan(block);

    assert_eq!(
        usize::try_from(metadata.note_count),
        Ok(notes),
        "notes, {context}"
    );
    assert_eq!(
        usize::try_from(metadata.sapling_tx_count),
        Ok(sapling_txs),
        "sapling txs, {context}"
    );
    assert_eq!(
        usize::try_from(metadata.orchard_tx_count),
        Ok(orchard_txs),
        "orchard txs, {context}"
    );
    assert_eq!(
        usize::try_from(metadata.ironwood_tx_count),
        Ok(ironwood_txs),
        "ironwood txs, {context}"
    );
}

/// Opens an ephemeral state and commits `blocks` to it.
///
/// The background format checks are skipped, because these tests delete
/// records to simulate a partial backfill, and run the upgrade directly.
fn state_with_blocks(network: &Network, blocks: &[Arc<Block>]) -> FinalizedState {
    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("opening an ephemeral database should succeed");

    for block in blocks {
        commit(&mut state, block);
    }

    state
}

fn commit(state: &mut FinalizedState, block: &Arc<Block>) {
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "sync metadata test",
        )
        .expect("test block is valid");
}

/// Deletes the records at and above `from`, as if an earlier backfill had
/// stopped there.
fn delete_records_from(db: &ZebraDb, from: Height) {
    let tip = db.finalized_tip_height().expect("test state has blocks");
    let mut batch = db.sync_meta_cf().new_batch_for_writing();
    for height in from.0..=tip.0 {
        batch = batch.zs_delete(&Height(height));
    }
    batch.write_batch().expect("deleting test records succeeds");
}

/// All records from genesis, in height order.
fn all_records(db: &ZebraDb) -> Vec<(Height, SyncMetadata)> {
    db.sync_meta_cf()
        .zs_forward_range_iter(Height(0)..)
        .collect()
}

fn run_upgrade(db: &ZebraDb) -> Result<(), CancelFormatChange> {
    let (_cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    let tip = db.finalized_tip_height().expect("test state has blocks");
    add_sync_metadata::Upgrade.run(tip, db, &cancel_receiver)
}

#[allow(clippy::unwrap_in_result)]
fn validate(db: &ZebraDb) -> Result<(), String> {
    let (_cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    add_sync_metadata::Upgrade
        .validate(db, &cancel_receiver)
        .expect("validation is not cancelled")
}

/// Every committed block's record matches a direct scan of the block, and the
/// cumulative transparent output count grows by exactly each block's output
/// count, starting from the genesis block's outputs.
#[test]
fn committed_records_match_a_block_scan() {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let blocks = test_blocks(&network);
        assert!(blocks.len() > 2, "{network} has test blocks");
        let state = state_with_blocks(&network, &blocks);

        let mut expected_cumulative: u64 = 0;
        for (height, block) in blocks.iter().enumerate() {
            let height = Height(height.try_into().expect("test heights fit in u32"));
            let metadata = state
                .db
                .sync_metadata(height)
                .expect("metadata is written when the block is committed");

            let block_outputs: usize = block.transactions.iter().map(|tx| tx.outputs().len()).sum();
            assert!(block_outputs > 0, "coinbase always creates outputs");
            // The cast is a lossless widening: output counts are far below u64.
            expected_cumulative += block_outputs as u64;

            let size = block
                .zcash_serialize_to_vec()
                .expect("test block serializes")
                .len();
            assert_eq!(
                usize::try_from(metadata.size),
                Ok(size),
                "{network} {height:?}"
            );
            assert_eq!(
                Some(metadata.size),
                state.db.block_info(height.into()).map(|info| info.size()),
                "the size matches the block info record",
            );
            assert_eq!(
                usize::try_from(metadata.tx_count),
                Ok(block.transactions.len())
            );
            assert_eq!(
                metadata.auth_data_root,
                <[u8; 32]>::from(block.auth_data_root())
            );
            assert_eq!(
                metadata.cumulative_transparent_outputs, expected_cumulative,
                "the cumulative count grows by the block's output count at {height:?} on {network}",
            );
            assert_shielded_counts_match_scan(&metadata, block, &format!("{network} {height:?}"));
        }

        assert!(state.db.sync_metadata_is_complete());
    }
}

/// The note commitment and per-pool transaction counts match a direct scan
/// of every test block vector, including blocks with Sprout, Sapling and
/// Orchard data. (No vector has Ironwood data yet, so the Ironwood counts are
/// only checked at zero.)
#[test]
fn shielded_counts_match_a_scan_of_every_test_vector() {
    let _init_guard = zebra_test::init();

    let (mut sprout_blocks, mut sapling_blocks, mut orchard_blocks) = (0, 0, 0);

    for network in Network::iter() {
        for (height, bytes) in network.block_iter() {
            let block: Block = bytes
                .zcash_deserialize_into()
                .expect("test data deserializes");
            let size = bytes.len();

            let metadata = SyncMetadata::for_block(&block, size, 0, None);
            assert_shielded_counts_match_scan(&metadata, &block, &format!("{network} {height}"));
            assert_eq!(usize::try_from(metadata.size), Ok(size));

            if block.transactions.iter().any(|tx| tx.joinsplit_count() > 0) {
                sprout_blocks += 1;
            }
            if metadata.sapling_tx_count > 0 {
                sapling_blocks += 1;
            }
            if metadata.orchard_tx_count > 0 {
                orchard_blocks += 1;
            }
        }
    }

    assert!(sprout_blocks > 0, "some test vectors have JoinSplits");
    assert!(sapling_blocks > 0, "some test vectors have Sapling data");
    assert!(orchard_blocks > 0, "some test vectors have Orchard data");
}

/// A precomputed authorizing data root is used as given, and without one the
/// root is computed from the block, so the checkpoint and contextual commit
/// paths write the same record.
#[test]
fn precomputed_auth_data_root_is_used() {
    let _init_guard = zebra_test::init();

    let block: Block = zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES
        .zcash_deserialize_into()
        .expect("test data deserializes");
    let size = zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES.len();
    let root = block.auth_data_root();

    let computed = SyncMetadata::for_block(&block, size, 7, None);
    assert_eq!(computed.auth_data_root, <[u8; 32]>::from(root));
    assert_eq!(
        SyncMetadata::for_block(&block, size, 7, Some(root)),
        computed
    );

    let fake = AuthDataRoot::from([0x5A; 32]);
    let with_fake = SyncMetadata::for_block(&block, size, 7, Some(fake));
    assert_eq!(with_fake.auth_data_root, [0x5A; 32]);
    assert_eq!(
        SyncMetadata {
            auth_data_root: computed.auth_data_root,
            ..with_fake
        },
        computed,
        "only the root comes from the precomputed value",
    );
}

/// Committing checkpoint blocks whose authorizing data root was precomputed
/// writes the same records as committing them without one.
#[test]
fn precomputed_root_commits_match_computed_root_commits() {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let blocks = test_blocks(&network);
        let expected = all_records(&state_with_blocks(&network, &blocks).db);

        let mut state = state_with_blocks(&network, &[]);
        for block in &blocks {
            state
                .commit_finalized_direct(
                    CheckpointVerifiedBlock::from(block.clone())
                        .with_precomputed_auth_data_root()
                        .into(),
                    None,
                    "sync metadata test",
                )
                .expect("test block is valid");
        }

        assert_eq!(all_records(&state.db), expected, "{network}");
    }
}

/// A backfill from scratch, and a backfill resumed after an earlier run
/// stopped part way, both write exactly the records commit time writes.
#[test]
fn upgrade_backfills_and_resumes() {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let blocks = test_blocks(&network);
        let state = state_with_blocks(&network, &blocks);
        let expected = all_records(&state.db);
        assert_eq!(expected.len(), blocks.len());

        for resume_from in [0, 1, blocks.len() / 2, blocks.len() - 1] {
            let resume_from = Height(resume_from.try_into().expect("test heights fit in u32"));
            delete_records_from(&state.db, resume_from);
            assert!(!state.db.sync_metadata_is_complete());
            assert_eq!(
                state.db.sync_metadata_range(Height(0)..=Height::MAX).len(),
                usize::try_from(resume_from.0).expect("fits"),
            );

            run_upgrade(&state.db).expect("the upgrade is not cancelled");

            assert_eq!(
                all_records(&state.db),
                expected,
                "resuming from {resume_from:?} on {network}"
            );
            assert!(state.db.sync_metadata_is_complete());
            assert_eq!(validate(&state.db), Ok(()));
        }
    }
}

/// A cancelled backfill writes nothing and returns `CancelFormatChange`.
#[test]
fn upgrade_stops_when_cancelled() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let blocks = test_blocks(&network);
    let state = state_with_blocks(&network, &blocks);
    delete_records_from(&state.db, Height(0));

    let (cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    cancel_sender
        .send(CancelFormatChange)
        .expect("the receiver is alive");
    let tip = state
        .db
        .finalized_tip_height()
        .expect("test state has blocks");

    assert_eq!(
        add_sync_metadata::Upgrade.run(tip, &state.db, &cancel_receiver),
        Err(CancelFormatChange),
    );
    assert!(all_records(&state.db).is_empty());
    assert!(validate(&state.db).is_err(), "an empty backfill is invalid");
}

/// While the backfill hasn't reached a block's parent, committing the block
/// writes no record, so the column family stays contiguous from genesis.
#[test]
fn commit_skips_the_record_while_the_backfill_is_behind() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let blocks = test_blocks(&network);
    let (last, below) = blocks.split_last().expect("test blocks exist");
    let mut state = state_with_blocks(&network, below);

    // No records at all: the backfill hasn't written genesis yet.
    delete_records_from(&state.db, Height(0));
    commit(&mut state, last);

    assert!(all_records(&state.db).is_empty());
    assert!(!state.db.sync_metadata_is_complete());
    assert!(state
        .db
        .sync_metadata_range(Height(0)..=Height::MAX)
        .is_empty());

    run_upgrade(&state.db).expect("the upgrade is not cancelled");
    assert_eq!(all_records(&state.db).len(), blocks.len());
    assert!(state.db.sync_metadata_is_complete());
}

/// A commit fills a short gap directly below it, which a commit racing the
/// end of the backfill leaves behind.
#[test]
fn commit_fills_a_short_tail_gap() {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let blocks = test_blocks(&network);
        let expected = all_records(&state_with_blocks(&network, &blocks).db);

        let (last, below) = blocks.split_last().expect("test blocks exist");
        let mut state = state_with_blocks(&network, below);

        let gap_start = Height(1);
        delete_records_from(&state.db, gap_start);
        assert_eq!(
            validate(&state.db),
            Ok(()),
            "a short gap below the tip is valid"
        );

        commit(&mut state, last);

        assert_eq!(all_records(&state.db), expected, "{network}");
        assert!(state.db.sync_metadata_is_complete());
    }
}

/// `sync_metadata_range` returns a contiguous prefix of the range, stopping
/// at the first missing height or at the tip.
#[test]
fn range_is_a_contiguous_prefix() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let blocks = test_blocks(&network);
    let state = state_with_blocks(&network, &blocks);
    let tip = state
        .db
        .finalized_tip_height()
        .expect("test state has blocks");
    let records: Vec<SyncMetadata> = all_records(&state.db)
        .into_iter()
        .map(|(_, metadata)| metadata)
        .collect();

    assert_eq!(state.db.sync_metadata_range(Height(0)..=tip), records);
    assert_eq!(
        state.db.sync_metadata_range(Height(1)..=Height::MAX),
        records[1..],
        "a range above the tip stops at the tip",
    );
    assert_eq!(
        state.db.sync_metadata_range(Height(2)..=Height(3)),
        records[2..=3]
    );
    assert!(state
        .db
        .sync_metadata_range(Height(tip.0 + 1)..=Height::MAX)
        .is_empty());
    assert_eq!(state.db.sync_metadata(Height(tip.0 + 1)), None);

    // A hole that can't happen in a valid database stops the range there,
    // even though later heights have records.
    state
        .db
        .sync_meta_cf()
        .new_batch_for_writing()
        .zs_delete(&Height(3))
        .write_batch()
        .expect("deleting a test record succeeds");

    assert_eq!(state.db.sync_metadata_range(Height(0)..=tip), records[..3]);
    assert!(state.db.sync_metadata_range(Height(3)..=tip).is_empty());
    assert_eq!(
        state.db.sync_metadata_range(Height(4)..=tip),
        records[4..],
        "a range starting above the hole is contiguous",
    );
}
