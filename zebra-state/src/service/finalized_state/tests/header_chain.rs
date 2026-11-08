//! Tests for the header chain column families.

use std::sync::Arc;

use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use crate::{
    service::finalized_state::{disk_db::DiskWriteBatch, FinalizedState, ZebraDb},
    CheckpointVerifiedBlock, Config,
};

/// Mainnet's contiguous test blocks from genesis.
fn test_blocks() -> Vec<Arc<Block>> {
    Network::Mainnet
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

fn empty_state() -> FinalizedState {
    FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &Network::Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("opening an ephemeral database should succeed")
}

fn commit(state: &mut FinalizedState, block: &Arc<Block>) {
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "header chain test",
        )
        .expect("test block is valid");
}

fn headers(blocks: &[Arc<Block>]) -> Vec<Arc<block::Header>> {
    blocks.iter().map(|block| block.header.clone()).collect()
}

fn append(db: &ZebraDb, first_height: Height, headers: &[Arc<block::Header>]) {
    let mut batch = DiskWriteBatch::new();
    batch.prepare_header_chain_append(db, first_height, headers);
    db.write_batch(batch).expect("test batch writes");
}

fn gc(db: &ZebraDb) -> usize {
    let mut batch = DiskWriteBatch::new();
    let deleted = batch.prepare_header_chain_gc(db);
    db.write_batch(batch).expect("test batch writes");
    deleted
}

fn stored_rows(db: &ZebraDb) -> Vec<Height> {
    db.header_by_height_cf()
        .zs_forward_range_iter(Height(0)..)
        .map(|(height, _)| height)
        .collect()
}

/// Headers stored above the finalized tip extend the header chain, and every
/// read stitches the finalized blocks and the stored headers together.
#[test]
fn stored_headers_extend_the_finalized_chain() {
    let _init_guard = zebra_test::init();

    let blocks = test_blocks();
    assert!(blocks.len() >= 6, "Mainnet has enough test blocks");

    let mut state = empty_state();
    assert_eq!(state.db.header_chain_tip(), None, "no genesis yet");

    for block in &blocks[..3] {
        commit(&mut state, block);
    }
    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(2), blocks[2].hash())),
        "without stored headers the header tip is the finalized tip",
    );
    assert_eq!(state.db.stored_header_tip(), None);

    append(&state.db, Height(3), &headers(&blocks[3..6]));

    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(5), blocks[5].hash()))
    );
    assert_eq!(
        state.db.stored_header_tip(),
        Some((Height(5), blocks[5].hash()))
    );

    for (height, block) in blocks[..6].iter().enumerate() {
        let height = Height(height.try_into().expect("test heights fit in u32"));
        assert_eq!(
            state.db.any_header(height),
            Some(block.header.clone()),
            "{height:?}"
        );
        assert_eq!(
            state.db.header_chain_hash(height),
            Some(block.hash()),
            "{height:?}"
        );
        assert_eq!(
            state.db.header_chain_height(block.hash()),
            Some(height),
            "{height:?}"
        );
    }

    assert_eq!(state.db.any_header(Height(6)), None);
    assert_eq!(state.db.header_chain_hash(Height(6)), None);

    // Stored headers are not blocks.
    assert!(!state.db.contains_hash(blocks[4].hash()));
    assert_eq!(state.db.block_header(Height(4).into()), None);
    assert_eq!(state.db.tip(), Some((Height(2), blocks[2].hash())));
}

/// A stored row at or below the finalized tip is stale: reads ignore it, even
/// when it holds a different header, and garbage collection deletes it with
/// its reverse index row.
#[test]
fn stale_rows_are_ignored_and_collected() {
    let _init_guard = zebra_test::init();

    let blocks = test_blocks();
    let mut state = empty_state();

    commit(&mut state, &blocks[0]);
    append(&state.db, Height(1), &headers(&blocks[1..5]));

    // Bodies commit without touching the header chain, as the block writer
    // does, so rows 1 and 2 become stale duplicates.
    commit(&mut state, &blocks[1]);
    commit(&mut state, &blocks[2]);

    // A conflicting stale row, as if the body chain finalized another branch.
    let mut batch = DiskWriteBatch::new();
    assert_eq!(batch.prepare_header_chain_truncate(&state.db, Height(2)), 3);
    state.db.write_batch(batch).expect("test batch writes");
    append(&state.db, Height(2), &[blocks[5].header.clone()]);
    append(&state.db, Height(3), &headers(&blocks[3..5]));

    assert_eq!(
        state.db.any_header(Height(2)),
        Some(blocks[2].header.clone())
    );
    assert_eq!(
        state.db.header_chain_hash(Height(2)),
        Some(blocks[2].hash())
    );
    assert_eq!(
        state.db.header_chain_height(blocks[5].hash()),
        None,
        "a stale reverse index row is not authoritative",
    );
    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(4), blocks[4].hash()))
    );

    assert_eq!(stored_rows(&state.db), [1, 2, 3, 4].map(Height));
    assert_eq!(gc(&state.db), 2, "rows 1 and 2 are stale");
    assert_eq!(stored_rows(&state.db), [3, 4].map(Height));
    assert_eq!(gc(&state.db), 0, "nothing is left to collect");

    for stale in [&blocks[1], &blocks[2], &blocks[5]] {
        assert_eq!(
            state.db.height_by_header_hash_cf().zs_get(&stale.hash()),
            None,
            "the reverse index row is deleted with its header row",
        );
    }

    // The header chain is unchanged.
    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(4), blocks[4].hash()))
    );
    assert_eq!(
        state.db.header_chain_height(blocks[1].hash()),
        Some(Height(1)),
        "committed blocks are found through height_by_hash",
    );
}

/// Once bodies pass every stored header, the header tip is the finalized tip.
#[test]
fn bodies_overtake_the_stored_headers() {
    let _init_guard = zebra_test::init();

    let blocks = test_blocks();
    let mut state = empty_state();

    commit(&mut state, &blocks[0]);
    append(&state.db, Height(1), &headers(&blocks[1..3]));

    for block in &blocks[1..4] {
        commit(&mut state, block);
    }

    assert_eq!(state.db.stored_header_tip(), None);
    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(3), blocks[3].hash()))
    );
    assert_eq!(gc(&state.db), 2);
    assert!(stored_rows(&state.db).is_empty());
}

/// Truncating deletes every row at or above a height, with its reverse index.
#[test]
fn truncate_deletes_rows_and_reverse_index() {
    let _init_guard = zebra_test::init();

    let blocks = test_blocks();
    let mut state = empty_state();

    commit(&mut state, &blocks[0]);
    append(&state.db, Height(1), &headers(&blocks[1..6]));

    let mut batch = DiskWriteBatch::new();
    assert_eq!(batch.prepare_header_chain_truncate(&state.db, Height(3)), 3);
    state.db.write_batch(batch).expect("test batch writes");

    assert_eq!(stored_rows(&state.db), [1, 2].map(Height));
    assert_eq!(
        state.db.header_chain_tip(),
        Some((Height(2), blocks[2].hash()))
    );
    for block in &blocks[3..6] {
        assert_eq!(
            state.db.height_by_header_hash_cf().zs_get(&block.hash()),
            None
        );
    }
}

/// If the block writer finalizes a different block just below stored headers,
/// reading the header chain stops at the row that doesn't link to it, until the
/// next stale-header collection drops those rows.
#[test]
fn header_chain_hashes_stop_at_a_disconnected_row() {
    let _init_guard = zebra_test::init();

    let blocks = test_blocks();
    let mut state = empty_state();
    for block in &blocks[..2] {
        commit(&mut state, block);
    }

    // A fork from height 2, stored after the header writer checked the finalized tip.
    let mut fork: Vec<Arc<block::Header>> = Vec::new();
    for block in &blocks[2..6] {
        let mut header = *block.header;
        if let Some(previous) = fork.last() {
            header.previous_block_hash = previous.hash();
        } else {
            header.time += chrono::Duration::seconds(1);
        }
        fork.push(Arc::new(header));
    }
    append(&state.db, Height(2), &fork);

    // The block writer finalizes the real block 2.
    commit(&mut state, &blocks[2]);

    assert_eq!(
        crate::service::read::header_chain_hashes(&state.db, &[blocks[2].hash()], 10),
        Vec::<block::Hash>::new(),
        "the stored rows don't link to the finalized block"
    );

    crate::service::header_write::collect_stale_headers(&state.db);
    assert!(stored_rows(&state.db).is_empty());
}

/// Reading the header chain stops at a row that doesn't link to the one below
/// it, so a read that races a header reorg returns one chain.
#[test]
fn header_chain_hashes_stop_at_a_link_break() {
    let _init_guard = zebra_test::init();

    let blocks = test_blocks();
    let mut state = empty_state();
    commit(&mut state, &blocks[0]);
    let db = &state.db;
    append(db, Height(1), &headers(&blocks[1..=3]));

    assert_eq!(
        crate::service::read::header_chain_hashes(db, &[blocks[1].hash()], 10),
        vec![blocks[2].hash(), blocks[3].hash()],
    );

    // A torn read: height 3 now holds a header from another branch.
    let mut fork = *blocks[3].header;
    fork.previous_block_hash = block::Hash([0xff; 32]);
    append(db, Height(3), &[Arc::new(fork)]);

    assert_eq!(
        crate::service::read::header_chain_hashes(db, &[blocks[1].hash()], 10),
        vec![blocks[2].hash()],
    );
}
