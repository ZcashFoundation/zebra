//! Fixed database test vectors for blocks and transactions.
//!
//! These tests check that the database correctly serializes
//! and deserializes large heights, blocks and transactions.
//!
//! # TODO
//!
//! Test large blocks and transactions with shielded data,
//! including data activated in Overwinter and later network upgrades.
//!
//! Check transparent address indexes, UTXOs, etc.

use std::{iter, path::Path, sync::Arc};

use zebra_chain::{
    block::{
        self,
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
            large_single_transaction_block_many_outputs,
        },
        Block, Height,
    },
    block_info::BlockInfo,
    parameters::{
        testnet,
        Network::{self, *},
        NetworkUpgrade,
    },
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent::new_ordered_outputs_with_height,
    work::difficulty::ParameterDifficulty,
};
use zebra_test::vectors::{MAINNET_BLOCKS, TESTNET_BLOCKS};

use crate::{
    constants::{
        state_database_format_version_in_code, MAX_BLOCK_REORG_HEIGHT,
        MAX_HEADER_SYNC_HEIGHT_RANGE, STATE_DATABASE_KIND,
    },
    error::CommitHeaderRangeError,
    request::{FinalizedBlock, Treestate},
    service::{
        check::difficulty::AdjustedDifficulty,
        finalized_state::{
            disk_db::{DiskWriteBatch, ReadDisk, WriteDisk},
            ZebraDb, PRUNING_METADATA, STATE_COLUMN_FAMILIES_IN_CODE,
        },
        read,
    },
    CheckpointVerifiedBlock, Config, SemanticallyVerifiedBlock, TransactionLocation,
};

/// Storage round-trip test for block and transaction data in the finalized state database.
#[test]
fn test_block_db_round_trip() {
    let mainnet_test_cases = MAINNET_BLOCKS
        .values()
        .map(|block| block.zcash_deserialize_into().unwrap());
    let testnet_test_cases = TESTNET_BLOCKS
        .values()
        .map(|block| block.zcash_deserialize_into().unwrap());

    test_block_db_round_trip_with(&Mainnet, mainnet_test_cases);
    test_block_db_round_trip_with(&Network::new_default_testnet(), testnet_test_cases);

    // It doesn't matter if these blocks are mainnet or testnet,
    // because there is no validation at this level of the database.
    //
    // These blocks have the same height and header hash, so they each need a new state.
    test_block_db_round_trip_with(&Mainnet, iter::once(large_multi_transaction_block()));

    // These blocks are unstable under serialization, so we apply a round-trip first.
    //
    // TODO: fix the bug in the generated test vectors.
    let block = large_single_transaction_block_many_inputs();
    let block_data = block
        .zcash_serialize_to_vec()
        .expect("serialization to vec never fails");
    let block: Block = block_data
        .zcash_deserialize_into()
        .expect("deserialization of valid serialized block never fails");
    test_block_db_round_trip_with(&Mainnet, iter::once(block));

    let block = large_single_transaction_block_many_outputs();
    let block_data = block
        .zcash_serialize_to_vec()
        .expect("serialization to vec never fails");
    let block: Block = block_data
        .zcash_deserialize_into()
        .expect("deserialization of valid serialized block never fails");
    test_block_db_round_trip_with(&Mainnet, iter::once(block));
}

#[test]
fn header_range_commit_keeps_body_availability_separate() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();

    let mut batch = DiskWriteBatch::new();
    let committed_hash = batch
        .prepare_header_range_batch(
            &state,
            genesis.hash(),
            std::slice::from_ref(&block1.header),
            &[0],
        )
        .expect("block 1 header links to genesis and has valid context");
    state
        .write_batch(batch)
        .expect("header range batch writes successfully");

    assert_eq!(committed_hash, block1.hash());
    assert_eq!(state.best_header_tip(), Some((Height(1), block1.hash())));
    assert_eq!(state.finalized_tip_height(), Some(Height(0)));
    assert_eq!(state.tip(), Some((Height(0), genesis.hash())));

    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
    assert_eq!(
        state.missing_block_bodies(Some(Height(0)), Some(Height(1)), Height(0), 10),
        vec![Height(1)],
    );

    assert!(!state.contains_hash(block1.hash()));
    assert!(!state.contains_body_at_height(Height(1)));
    assert!(state.block(Height(1).into()).is_none());
    assert!(state
        .transaction_hashes_for_block(Height(1).into())
        .is_none());
}

#[test]
fn header_range_commit_stores_advertised_body_sizes_with_zero_as_unknown() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();
    let block2 = mainnet_block(2);

    let headers = vec![block1.header.clone(), block2.header.clone()];
    let mut batch = DiskWriteBatch::new();
    batch
        .prepare_header_range_batch(&state, genesis.hash(), &headers, &[123_456, 0])
        .expect("block headers link to genesis and have valid context");
    state
        .write_batch(batch)
        .expect("header range batch writes successfully");

    assert_eq!(state.advertised_body_size(Height(1)), Some(123_456));
    assert_eq!(state.advertised_body_size(Height(2)), None);
}

#[test]
fn block_size_hints_prefer_confirmed_block_info_over_advertised_hint() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();

    let mut batch = DiskWriteBatch::new();
    batch
        .prepare_header_range_batch(
            &state,
            genesis.hash(),
            std::slice::from_ref(&block1.header),
            &[999_999],
        )
        .expect("block 1 header links to genesis and has valid context");
    state
        .write_batch(batch)
        .expect("header range batch writes successfully");
    assert_eq!(state.advertised_body_size(Height(1)), Some(999_999));

    write_full_block_header_and_transactions(&state, block1.clone());
    let block1_size = u32::try_from(block1.zcash_serialize_to_vec().unwrap().len())
        .expect("serialized block size fits in u32");
    let mut block_info_batch = DiskWriteBatch::new();
    let _ = state
        .block_info_cf()
        .with_batch_for_writing(&mut block_info_batch)
        .zs_insert(&Height(1), &BlockInfo::new(Default::default(), block1_size));
    state
        .db
        .write(block_info_batch)
        .expect("block info batch writes successfully");

    assert_eq!(
        crate::service::read::block_size_hints(
            None::<Arc<crate::service::non_finalized_state::Chain>>,
            &state,
            Height(1),
            1,
        ),
        vec![(Height(1), Some(block1_size))],
    );
}

#[test]
fn header_range_read_is_contiguous_capped_and_stops_at_first_gap() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();

    let mut batch = DiskWriteBatch::new();
    batch
        .prepare_header_range_batch(
            &state,
            genesis.hash(),
            std::slice::from_ref(&block1.header),
            &[0],
        )
        .expect("block 1 header is valid");
    state.write_batch(batch).expect("header batch writes");

    assert_eq!(state.headers_by_height_range(Height(0), 3).len(), 2);
    assert_eq!(
        state.headers_by_height_range(Height(1), u32::MAX),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
    assert!(state.headers_by_height_range(Height(2), 10).is_empty());
}

#[test]
fn header_range_read_enforces_max_range_cap() {
    let _init_guard = zebra_test::init();
    let (state, _genesis, block1) = mainnet_state_with_genesis();
    let header_by_height = state.db.cf_handle("zakura_header_by_height").unwrap();
    let hash_by_height = state.db.cf_handle("zakura_header_hash_by_height").unwrap();

    let mut batch = DiskWriteBatch::new();
    for height in 1..=MAX_HEADER_SYNC_HEIGHT_RANGE + 1 {
        batch.zs_insert(&header_by_height, Height(height), &block1.header);
        batch.zs_insert(&hash_by_height, Height(height), block1.hash());
    }
    state.db.write(batch).expect("header test rows write");

    assert_eq!(
        state.headers_by_height_range(Height(1), u32::MAX).len(),
        usize::try_from(MAX_HEADER_SYNC_HEIGHT_RANGE).expect("range cap fits in usize"),
    );
}

#[test]
fn missing_block_bodies_respects_from_limit_and_empty_body_gap() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();
    let block2 = mainnet_block(2);
    let block3 = mainnet_block(3);

    assert!(state
        .missing_block_bodies(Some(Height(0)), Some(Height(0)), Height(0), 10)
        .is_empty());

    commit_header_range(
        &state,
        genesis.hash(),
        &[
            block1.header.clone(),
            block2.header.clone(),
            block3.header.clone(),
        ],
    );

    assert_eq!(
        state.missing_block_bodies(Some(Height(0)), Some(Height(3)), Height(2), 1),
        vec![Height(2)],
    );
    assert_eq!(
        state.missing_block_bodies(Some(Height(0)), Some(Height(3)), Height(2), 10),
        vec![Height(2), Height(3)],
    );
}

#[test]
fn committed_block_does_not_retain_zakura_header() {
    let _init_guard = zebra_test::init();
    let (state, _genesis, block1) = mainnet_state_with_genesis_and_zakura_seed();
    let zakura_header_by_height = state.db.cf_handle("zakura_header_by_height").unwrap();

    assert!(state.headers_by_height_range(Height(1), 1).is_empty());

    write_full_block_header_and_transactions(&state, block1.clone());

    // The committed body's header is served from the consensus column families,
    // and the Zakura header store keeps no row at a height that has a body.
    assert_eq!(state.best_header_tip(), Some((Height(1), block1.hash())));
    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
    assert!(state
        .db
        .zs_get::<_, _, Arc<block::Header>>(&zakura_header_by_height, &Height(1))
        .is_none());
}

#[test]
fn committed_block_does_not_seed_zakura_header_by_default() {
    let _init_guard = zebra_test::init();
    let (state, _genesis, block1) = mainnet_state_with_genesis();
    let zakura_header_by_height = state.db.cf_handle("zakura_header_by_height").unwrap();

    assert!(state.headers_by_height_range(Height(1), 1).is_empty());
    assert!(state
        .db
        .zs_get::<_, _, Arc<block::Header>>(&zakura_header_by_height, &Height(1))
        .is_none());

    write_full_block_header_and_transactions(&state, block1);

    assert!(state
        .db
        .zs_get::<_, _, Arc<block::Header>>(&zakura_header_by_height, &Height(1))
        .is_none());
}

#[test]
fn committed_block_releases_and_truncates_mismatched_zakura_headers() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis_and_zakura_seed();
    let block2 = mainnet_block(2);
    let old_header1 = alternate_header(genesis.hash(), &block1.header, 1);
    let old_hash1 = block::Hash::from(&*old_header1);
    let old_header2 = alternate_header(old_hash1, &block2.header, 2);
    let old_hash2 = block::Hash::from(&*old_header2);

    let header_by_height = state.db.cf_handle("zakura_header_by_height").unwrap();
    let hash_by_height = state.db.cf_handle("zakura_header_hash_by_height").unwrap();
    let height_by_hash = state.db.cf_handle("zakura_header_height_by_hash").unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.zs_insert(&header_by_height, Height(1), &old_header1);
    batch.zs_insert(&hash_by_height, Height(1), old_hash1);
    batch.zs_insert(&height_by_hash, old_hash1, Height(1));
    batch.zs_insert(&header_by_height, Height(2), &old_header2);
    batch.zs_insert(&hash_by_height, Height(2), old_hash2);
    batch.zs_insert(&height_by_hash, old_hash2, Height(2));
    state.db.write(batch).expect("mismatched headers write");

    assert_eq!(
        state.headers_by_height_range(Height(1), 2),
        vec![
            (Height(1), old_hash1, old_header1),
            (Height(2), old_hash2, old_header2),
        ],
    );

    write_full_block_header_and_transactions(&state, block1.clone());

    assert_eq!(state.best_header_tip(), Some((Height(1), block1.hash())));
    assert_eq!(
        state.headers_by_height_range(Height(1), 2),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
    // The conflicting provisional row at H1 is released and its stale descendant
    // at H2 is truncated; neither survives in the Zakura header store.
    assert!(state
        .db
        .zs_get::<_, _, Arc<block::Header>>(&header_by_height, &Height(1))
        .is_none());
    assert!(state
        .db
        .zs_get::<_, _, Arc<block::Header>>(&header_by_height, &Height(2))
        .is_none());
}

#[test]
fn committed_block_releases_matching_zakura_header() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis_and_zakura_seed();
    let zakura_header_by_height = state.db.cf_handle("zakura_header_by_height").unwrap();

    commit_header_range(&state, genesis.hash(), std::slice::from_ref(&block1.header));

    assert_eq!(state.best_header_tip(), Some((Height(1), block1.hash())));
    write_full_block_header_and_transactions(&state, block1.clone());

    // Committing the matching body releases the provisional row; the header is
    // now served from the consensus column families.
    assert_eq!(state.best_header_tip(), Some((Height(1), block1.hash())));
    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
    assert!(state
        .db
        .zs_get::<_, _, Arc<block::Header>>(&zakura_header_by_height, &Height(1))
        .is_none());
}

/// Committing a body at the bottom of a header-only frontier releases just that
/// height from the Zakura header store while the higher frontier rows remain,
/// and reads span the body/frontier boundary.
#[test]
fn committed_body_releases_only_its_height_and_keeps_the_frontier() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis_and_zakura_seed();
    let block2 = mainnet_block(2);
    let zakura_header_by_height = state.db.cf_handle("zakura_header_by_height").unwrap();

    commit_header_range(
        &state,
        genesis.hash(),
        &[block1.header.clone(), block2.header.clone()],
    );
    assert_eq!(state.best_header_tip(), Some((Height(2), block2.hash())));

    write_full_block_header_and_transactions(&state, block1.clone());

    // H1 is released (its body is committed); the H2 frontier row survives.
    assert!(state
        .db
        .zs_get::<_, _, Arc<block::Header>>(&zakura_header_by_height, &Height(1))
        .is_none());
    assert_eq!(
        state
            .db
            .zs_get::<_, _, Arc<block::Header>>(&zakura_header_by_height, &Height(2)),
        Some(block2.header.clone()),
    );

    // best_header_tip still reflects the frontier, and the contiguous read spans
    // the committed body (H1, from the consensus CFs) and the frontier (H2).
    assert_eq!(state.best_header_tip(), Some((Height(2), block2.hash())));
    assert_eq!(
        state.headers_by_height_range(Height(1), 2),
        vec![
            (Height(1), block1.hash(), block1.header.clone()),
            (Height(2), block2.hash(), block2.header.clone()),
        ],
    );
}

/// Pruning-readiness guard: a committed height whose body is removed (as online
/// pruning deletes `tx_by_loc` rows) keeps its header readable from the retained
/// consensus `block_header_by_height`, because the header readers are not gated
/// on body availability.
#[test]
fn header_stays_readable_after_body_is_pruned() {
    let _init_guard = zebra_test::init();
    let (state, _genesis, block1) = mainnet_state_with_genesis();

    write_full_block_header_and_transactions(&state, block1.clone());
    assert!(state.contains_body_at_height(Height(1)));
    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );

    // Simulate online pruning, which range-deletes the body's `tx_by_loc` rows
    // (including the coinbase) but retains the header/hash/height indexes.
    let tx_by_loc = state.db.cf_handle("tx_by_loc").unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &tx_by_loc,
        TransactionLocation::min_for_height(Height(1)),
        TransactionLocation::min_for_height(Height(2)),
    );
    state.db.write(batch).expect("body prune writes");

    // The body is gone, but the header is still served from the consensus CFs.
    assert!(!state.contains_body_at_height(Height(1)));
    assert!(state.block(Height(1).into()).is_none());
    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
}

/// `Request::KnownBlock` must report a finalized block as known even after its body
/// is pruned: pruning removes `tx_by_loc` but keeps the hash index, and membership is
/// decided by the hash index, not body availability (F-88603). Otherwise sync and
/// inbound gossip re-download a body we already finalized only to reject it as behind
/// the finalized tip.
#[test]
fn known_block_reports_finalized_block_after_body_pruned() {
    let _init_guard = zebra_test::init();
    let (state, _genesis, block1) = mainnet_state_with_genesis();

    write_full_block_header_and_transactions(&state, block1.clone());
    assert!(state.contains_body_at_height(Height(1)));
    assert_eq!(
        read::finalized_state_contains_block_hash(&state, block1.hash()),
        Some(crate::KnownBlock::Finalized),
    );

    // Prune the body (range-delete `tx_by_loc` for height 1), keeping the hash index.
    let tx_by_loc = state.db.cf_handle("tx_by_loc").unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &tx_by_loc,
        TransactionLocation::min_for_height(Height(1)),
        TransactionLocation::min_for_height(Height(2)),
    );
    state.db.write(batch).expect("body prune writes");

    // The body-availability predicate (what KnownBlock used before the fix) now
    // reports the block as absent...
    assert!(!state.contains_body_at_height(Height(1)));
    assert!(!state.contains_hash(block1.hash()));

    // ...but KnownBlock still reports it as a known finalized block.
    assert_eq!(
        read::finalized_state_contains_block_hash(&state, block1.hash()),
        Some(crate::KnownBlock::Finalized),
    );
}

#[test]
fn header_range_commit_rejects_finalized_or_body_conflicts() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();
    write_full_block_header_and_transactions(&state, block1.clone());

    let mut conflicting = *block1.header;
    conflicting.nonce.0[0] ^= 1;

    let mut batch = DiskWriteBatch::new();
    assert!(matches!(
        batch.prepare_header_range_batch(&state, genesis.hash(), &[Arc::new(conflicting)], &[0]),
        Err(CommitHeaderRangeError::ImmutableConflict { height: Height(1) })
            | Err(CommitHeaderRangeError::ConflictingFullBlockHeader { height: Height(1) })
    ));
}

#[test]
fn header_range_commit_rejects_checkpoint_conflicts() {
    let _init_guard = zebra_test::init();
    let genesis = mainnet_block(0);
    let block1 = mainnet_block(1);
    let network = checkpoint_test_network(genesis.hash(), block1.hash());
    let state = state_with_genesis(&network, genesis.clone());

    let mut forged = *block1.header;
    forged.nonce.0[0] ^= 1;

    let mut batch = DiskWriteBatch::new();
    assert!(matches!(
        batch.prepare_header_range_batch(&state, genesis.hash(), &[Arc::new(forged)], &[0]),
        Err(CommitHeaderRangeError::CheckpointConflict {
            height: Height(1),
            expected,
            actual,
        }) if expected == block1.hash() && actual != expected
    ));

    assert_eq!(state.best_header_tip(), Some((Height(0), genesis.hash())));
}

#[test]
fn header_range_commit_accepts_matching_checkpoint_hash() {
    let _init_guard = zebra_test::init();
    let genesis = mainnet_block(0);
    let block1 = mainnet_block(1);
    let network = checkpoint_test_network(genesis.hash(), block1.hash());
    let state = state_with_genesis(&network, genesis.clone());

    let committed_hash =
        commit_header_range(&state, genesis.hash(), std::slice::from_ref(&block1.header));

    assert_eq!(committed_hash, block1.hash());
    assert_eq!(state.best_header_tip(), Some((Height(1), block1.hash())));
    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
}

/// A conflicting header range with *less* cumulative work than the chain it would
/// overwrite must be rejected, leaving the existing higher-work chain intact (the
/// F-88605 most-work guard). Before that guard, a single height-2 header replaced a
/// height-3 chain purely because it conflicted, silently dropping height 3.
#[test]
fn header_range_reorg_rejects_shorter_lower_work_range() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();
    let block2 = mainnet_block(2);
    let block3 = mainnet_block(3);

    commit_header_range(
        &state,
        genesis.hash(),
        &[
            block1.header.clone(),
            block2.header.clone(),
            block3.header.clone(),
        ],
    );

    // A single replacement header at height 2 conflicts with the stored chain but,
    // dropping height 3, carries strictly less cumulative work than {block2, block3}.
    let alternate_block2 = alternate_header(block1.hash(), &block2.header, 1);

    let mut batch = DiskWriteBatch::new();
    assert!(
        matches!(
            batch.prepare_header_range_batch(
                &state,
                block1.hash(),
                std::slice::from_ref(&alternate_block2),
                &[0],
            ),
            Err(CommitHeaderRangeError::LowerWorkConflict { height, .. }) if height == Height(2)
        ),
        "a shorter lower-work conflicting range must be rejected"
    );

    // The existing higher-work chain is untouched.
    assert_eq!(state.best_header_tip(), Some((Height(3), block3.hash())),);
    assert_eq!(
        state.headers_by_height_range(Height(1), 3),
        vec![
            (Height(1), block1.hash(), block1.header.clone()),
            (Height(2), block2.hash(), block2.header.clone()),
            (Height(3), block3.hash(), block3.header.clone()),
        ],
    );
}

/// A conflicting header range carrying the *same* cumulative work as the existing
/// chain (an equal-length fork) is also rejected: the incumbent chain wins ties, so
/// equal-work peers cannot churn the best header chain. Both chains are synthetic so
/// their per-height difficulties — and therefore cumulative work — are identical by
/// construction.
#[test]
fn header_range_reorg_rejects_equal_work_range() {
    let _init_guard = zebra_test::init();
    let genesis = mainnet_block(0);
    let network = no_extra_checkpoint_test_network(genesis.hash());
    let state = state_with_genesis(&network, genesis.clone());

    let original = synthetic_headers_from_state(&state, Height(0), genesis.hash(), 2, 1);
    let original_tip = block::Hash::from(&**original.last().unwrap());
    commit_header_range(&state, genesis.hash(), &original);

    // A different equal-length fork conflicting at height 1.
    let fork = synthetic_headers_from_state(&state, Height(0), genesis.hash(), 2, 9);

    let mut batch = DiskWriteBatch::new();
    assert!(
        matches!(
            batch.prepare_header_range_batch(&state, genesis.hash(), &fork, &[0, 0]),
            Err(CommitHeaderRangeError::LowerWorkConflict { height, existing_work, new_work })
                if height == Height(1) && new_work == existing_work
        ),
        "an equal-work conflicting range must be rejected (incumbent wins ties)"
    );

    assert_eq!(state.best_header_tip(), Some((Height(2), original_tip)));
}

#[test]
fn header_range_reorg_replaces_longer_range_without_stale_indexes() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();
    let block2 = mainnet_block(2);
    let block3 = mainnet_block(3);

    commit_header_range(
        &state,
        genesis.hash(),
        &[block1.header.clone(), block2.header.clone()],
    );

    let old_block2_hash = block2.hash();
    let alternate_block2 = alternate_header(block1.hash(), &block2.header, 1);
    let alternate_block2_hash = block::Hash::from(&*alternate_block2);
    let alternate_block3 = alternate_header(alternate_block2_hash, &block3.header, 2);
    let alternate_block3_hash = block::Hash::from(&*alternate_block3);

    let committed_hash = commit_header_range(
        &state,
        block1.hash(),
        &[alternate_block2.clone(), alternate_block3.clone()],
    );

    assert_eq!(committed_hash, alternate_block3_hash);
    assert_eq!(
        state.best_header_tip(),
        Some((Height(3), alternate_block3_hash))
    );
    assert_eq!(state.height(old_block2_hash), None);
    assert!(state.block(old_block2_hash.into()).is_none());

    assert_eq!(
        state.headers_by_height_range(Height(1), 4),
        vec![
            (Height(1), block1.hash(), block1.header.clone()),
            (Height(2), alternate_block2_hash, alternate_block2),
            (Height(3), alternate_block3_hash, alternate_block3),
        ],
    );
}

#[test]
fn header_range_reorg_rejects_too_deep_overwrite() {
    let _init_guard = zebra_test::init();
    let genesis = mainnet_block(0);
    let network = no_extra_checkpoint_test_network(genesis.hash());
    let state = state_with_genesis(&network, genesis.clone());
    let tip_height = Height(MAX_BLOCK_REORG_HEIGHT + 1);
    let original_headers =
        synthetic_headers_from_state(&state, Height(0), genesis.hash(), tip_height.0, 1);

    commit_header_range(&state, genesis.hash(), &original_headers);

    let conflict_height = Height(1);
    let conflict_anchor = genesis.hash();
    let conflicting_header = synthetic_headers_from_state(&state, Height(0), conflict_anchor, 1, 2)
        .pop()
        .expect("one synthetic conflicting header was generated");

    let mut batch = DiskWriteBatch::new();
    assert!(matches!(
        batch.prepare_header_range_batch(&state, conflict_anchor, &[conflicting_header], &[0]),
        Err(CommitHeaderRangeError::ReorgTooDeep {
            height,
            best_header_tip,
        }) if height == conflict_height && best_header_tip == tip_height
    ));

    assert_eq!(
        state.best_header_tip(),
        Some((
            tip_height,
            block::Hash::from(&**original_headers.last().unwrap())
        )),
    );
}

#[test]
fn header_range_reorg_accepts_boundary_adjacent_overwrite() {
    let _init_guard = zebra_test::init();
    let genesis = mainnet_block(0);
    let network = no_extra_checkpoint_test_network(genesis.hash());
    let state = state_with_genesis(&network, genesis.clone());
    let tip_height = Height(MAX_BLOCK_REORG_HEIGHT);
    let original_headers =
        synthetic_headers_from_state(&state, Height(0), genesis.hash(), tip_height.0, 1);

    commit_header_range(&state, genesis.hash(), &original_headers);

    let conflict_height = Height(1);
    // The reorg starts at the maximum allowed depth (height 1, exactly the boundary
    // from a best header tip of MAX_BLOCK_REORG_HEIGHT). With the most-work guard it
    // must also carry strictly more work than the chain it replaces, so use a fork
    // that is one block longer.
    let replacement =
        synthetic_headers_from_state(&state, Height(0), genesis.hash(), tip_height.0 + 1, 2);
    let new_conflict_hash = block::Hash::from(&*replacement[0]);
    let new_tip_height = Height(tip_height.0 + 1);
    let new_tip_hash = block::Hash::from(&**replacement.last().unwrap());

    let committed_hash = commit_header_range(&state, genesis.hash(), &replacement);

    assert_eq!(committed_hash, new_tip_hash);
    assert_eq!(
        state.best_header_tip(),
        Some((new_tip_height, new_tip_hash)),
    );
    // The boundary conflict height was overwritten with the higher-work fork.
    let height_1 = state.headers_by_height_range(conflict_height, 1);
    assert_eq!(height_1.len(), 1);
    assert_eq!(height_1[0].0, conflict_height);
    assert_eq!(height_1[0].1, new_conflict_hash);
}

#[test]
fn header_range_commit_rejects_non_current_anchor_hash() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();
    let stale_anchor = block::Hash([0x42; 32]);
    let height_by_hash = state.db.cf_handle("height_by_hash").unwrap();

    commit_header_range(&state, genesis.hash(), std::slice::from_ref(&block1.header));

    let mut stale_index_batch = DiskWriteBatch::new();
    stale_index_batch.zs_insert(&height_by_hash, stale_anchor, Height(1));
    state
        .db
        .write(stale_index_batch)
        .expect("stale test index writes");

    let block2 = mainnet_block(2);
    let alternate_block2 = alternate_header(stale_anchor, &block2.header, 1);

    let mut batch = DiskWriteBatch::new();
    assert!(matches!(
        batch.prepare_header_range_batch(&state, stale_anchor, &[alternate_block2], &[0]),
        Err(CommitHeaderRangeError::UnknownAnchor { anchor }) if anchor == stale_anchor
    ));

    assert_eq!(state.hash(Height(1)), None);
    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
}

#[test]
fn header_range_commit_reports_missing_genesis_before_scratch_bootstrap() {
    let _init_guard = zebra_test::init();
    let genesis = mainnet_block(0);
    let block1 = mainnet_block(1);
    let state = ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    );

    let mut batch = DiskWriteBatch::new();
    assert!(matches!(
        batch.prepare_header_range_batch(
            &state,
            genesis.hash(),
            std::slice::from_ref(&block1.header),
            &[0],
        ),
        Err(CommitHeaderRangeError::MissingGenesisAnchor { anchor }) if anchor == genesis.hash()
    ));

    assert_eq!(state.best_header_tip(), None);
}

#[test]
fn header_range_rows_and_tip_survive_reopen_without_body_availability() {
    let _init_guard = zebra_test::init();
    let tempdir = tempfile::tempdir().expect("temporary cache directory is created");
    let cache_dir = tempdir.path().to_owned();
    let config = persistent_config(&cache_dir);
    let genesis = mainnet_block(0);
    let block1 = mainnet_block(1);
    let block2 = mainnet_block(2);

    {
        let state = persistent_state(&config, &Mainnet);
        write_full_block_header_and_transactions(&state, genesis.clone());
        commit_header_range(
            &state,
            genesis.hash(),
            &[block1.header.clone(), block2.header.clone()],
        );

        assert_eq!(state.best_header_tip(), Some((Height(2), block2.hash())));
        let mut state = state;
        state.shutdown(true);
    }

    let reopened = persistent_state(&config, &Mainnet);

    assert_eq!(reopened.tip(), Some((Height(0), genesis.hash())));
    assert_eq!(reopened.best_header_tip(), Some((Height(2), block2.hash())));
    assert_eq!(
        reopened.headers_by_height_range(Height(1), 2),
        vec![
            (Height(1), block1.hash(), block1.header.clone()),
            (Height(2), block2.hash(), block2.header.clone()),
        ],
    );
    assert!(!reopened.contains_hash(block2.hash()));
    assert_eq!(reopened.body_hash(Height(2)), None);
    assert!(reopened.block(Height(2).into()).is_none());
}

#[test]
fn block_facing_hash_by_height_requires_body_availability() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();

    commit_header_range(&state, genesis.hash(), std::slice::from_ref(&block1.header));

    assert_eq!(state.hash(Height(1)), None);
    assert_eq!(state.body_hash(Height(1)), None);
    assert_eq!(
        read::hash_by_height(
            Option::<Arc<crate::service::non_finalized_state::Chain>>::None,
            &state,
            Height(1),
        ),
        None,
    );
    assert_eq!(
        state.headers_by_height_range(Height(1), 1),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
}

#[test]
fn full_block_commit_over_identical_header_only_row_is_noop_for_header_indexes() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis();

    commit_header_range(&state, genesis.hash(), std::slice::from_ref(&block1.header));

    assert_eq!(state.hash(Height(1)), None);
    assert_eq!(state.height(block1.hash()), None);
    assert!(!state.contains_hash(block1.hash()));
    assert_eq!(
        state.headers_by_height_range(Height(1), 2),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );

    write_full_block_header_and_transactions(&state, block1.clone());

    assert_eq!(state.hash(Height(1)), Some(block1.hash()));
    assert_eq!(state.height(block1.hash()), Some(Height(1)));
    assert!(state.contains_hash(block1.hash()));
    assert_eq!(
        state.headers_by_height_range(Height(1), 2),
        vec![(Height(1), block1.hash(), block1.header.clone())],
    );
}

#[test]
fn full_block_commit_overwrites_conflicting_header_only_rows() {
    let _init_guard = zebra_test::init();
    let (state, genesis, block1) = mainnet_state_with_genesis_and_zakura_seed();
    let block2 = mainnet_block(2);
    let block3 = mainnet_block(3);

    commit_header_range(&state, genesis.hash(), std::slice::from_ref(&block1.header));

    let alternate_block2 = alternate_header(block1.hash(), &block2.header, 1);
    let alternate_block2_hash = block::Hash::from(&*alternate_block2);
    let alternate_block3 = alternate_header(alternate_block2_hash, &block3.header, 2);
    let alternate_block3_hash = block::Hash::from(&*alternate_block3);
    commit_header_range(&state, block1.hash(), &[alternate_block2, alternate_block3]);

    write_full_block_header_and_transactions(&state, block2.clone());

    assert_eq!(state.hash(Height(2)), Some(block2.hash()));
    assert_eq!(state.height(alternate_block2_hash), None);
    assert_eq!(state.height(alternate_block3_hash), None);
    assert_eq!(state.best_header_tip(), Some((Height(2), block2.hash())));
    assert!(state.contains_hash(block2.hash()));
    assert!(state.block(block2.hash().into()).is_some());
    assert!(state.block(alternate_block2_hash.into()).is_none());
}

fn mainnet_state_with_genesis() -> (ZebraDb, Arc<Block>, Arc<Block>) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("genesis block deserializes");
    let block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 1 deserializes");
    let state = state_with_genesis(&Mainnet, genesis.clone());

    (state, genesis, block1)
}

fn mainnet_state_with_genesis_and_zakura_seed() -> (ZebraDb, Arc<Block>, Arc<Block>) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("genesis block deserializes");
    let block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 1 deserializes");
    let state = state_with_genesis_and_zakura_seed(&Mainnet, genesis.clone());

    (state, genesis, block1)
}

fn state_with_genesis(network: &Network, genesis: Arc<Block>) -> ZebraDb {
    state_with_genesis_config(network, genesis, Config::ephemeral())
}

fn state_with_genesis_and_zakura_seed(network: &Network, genesis: Arc<Block>) -> ZebraDb {
    let mut config = Config::ephemeral();
    config.enable_zakura_header_seed_from_committed_blocks = true;

    state_with_genesis_config(network, genesis, config)
}

fn state_with_genesis_config(network: &Network, genesis: Arc<Block>, config: Config) -> ZebraDb {
    let state = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    );

    write_full_block_header_and_transactions(&state, genesis.clone());

    state
}

fn persistent_config(cache_dir: &Path) -> Config {
    Config {
        cache_dir: cache_dir.to_owned(),
        ephemeral: false,
        debug_skip_non_finalized_state_backup_task: true,
        ..Config::default()
    }
}

fn persistent_state(config: &Config, network: &Network) -> ZebraDb {
    ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
}

fn checkpoint_test_network(genesis_hash: block::Hash, checkpoint_hash: block::Hash) -> Network {
    testnet::Parameters::build()
        .with_network_name("HeaderCheckpointTest")
        .expect("test network name is valid")
        .with_genesis_hash(genesis_hash)
        .expect("test genesis hash is valid")
        .with_target_difficulty_limit(Mainnet.target_difficulty_limit())
        .expect("mainnet difficulty limit is valid for test network")
        .with_activation_heights(testnet::ConfiguredActivationHeights {
            canopy: Some(2),
            ..Default::default()
        })
        .expect("test activation heights are valid")
        .clear_funding_streams()
        .with_checkpoints(testnet::ConfiguredCheckpoints::HeightsAndHashes(vec![
            (Height(0), genesis_hash),
            (Height(1), checkpoint_hash),
        ]))
        .expect("test checkpoints are valid")
        .to_network()
        .expect("test network is valid")
}

fn no_extra_checkpoint_test_network(genesis_hash: block::Hash) -> Network {
    testnet::Parameters::build()
        .with_network_name("HeaderReorgTest")
        .expect("test network name is valid")
        .with_genesis_hash(genesis_hash)
        .expect("test genesis hash is valid")
        .with_target_difficulty_limit(Mainnet.target_difficulty_limit())
        .expect("mainnet difficulty limit is valid for test network")
        .with_activation_heights(testnet::ConfiguredActivationHeights {
            canopy: Some(1),
            ..Default::default()
        })
        .expect("test activation heights are valid")
        .clear_funding_streams()
        .clear_checkpoints()
        .expect("genesis-only checkpoints are valid")
        .to_network()
        .expect("test network is valid")
}

fn mainnet_block(height: u32) -> Arc<Block> {
    MAINNET_BLOCKS
        .get(&height)
        .expect("test vector exists")
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("mainnet test block deserializes")
}

fn synthetic_headers_from_state(
    state: &ZebraDb,
    anchor_height: Height,
    anchor_hash: block::Hash,
    count: u32,
    nonce_seed: u8,
) -> Vec<Arc<block::Header>> {
    let network = state.network();
    let template = mainnet_block(1);
    let mut context = state.recent_header_context(anchor_height);
    let mut previous_hash = anchor_hash;
    let mut previous_height = anchor_height;
    let mut nonce_tag = nonce_seed;

    (0..count)
        .map(|_| {
            let candidate_height = previous_height
                .next()
                .expect("test header height remains in range");
            let previous_time = context
                .first()
                .expect("anchor header context is available")
                .1;
            let target_spacing =
                NetworkUpgrade::target_spacing_for_height(&network, candidate_height);
            let candidate_time = previous_time + target_spacing;
            let expected_difficulty = AdjustedDifficulty::new_from_header_time(
                candidate_time,
                previous_height,
                &network,
                context.iter().copied(),
            )
            .expected_difficulty_threshold();

            let mut header = *template.header;
            header.previous_block_hash = previous_hash;
            header.time = candidate_time;
            header.difficulty_threshold = expected_difficulty;
            header.nonce.0[0] = header.nonce.0[0].wrapping_add(nonce_tag);
            nonce_tag = nonce_tag.wrapping_add(1);

            let header = Arc::new(header);
            previous_hash = block::Hash::from(&*header);
            previous_height = candidate_height;
            context.insert(0, (header.difficulty_threshold, header.time));
            context.truncate(crate::service::check::difficulty::POW_ADJUSTMENT_BLOCK_SPAN);
            header
        })
        .collect()
}

fn alternate_header(
    previous_block_hash: block::Hash,
    template: &Arc<block::Header>,
    nonce_tag: u8,
) -> Arc<block::Header> {
    let mut header = **template;
    header.previous_block_hash = previous_block_hash;
    header.nonce.0[0] ^= nonce_tag;
    Arc::new(header)
}

fn commit_header_range(
    state: &ZebraDb,
    anchor: block::Hash,
    headers: &[Arc<block::Header>],
) -> block::Hash {
    let mut batch = DiskWriteBatch::new();
    let body_sizes = vec![0; headers.len()];
    let committed_hash = batch
        .prepare_header_range_batch(state, anchor, headers, &body_sizes)
        .expect("header range is valid");
    state
        .write_batch(batch)
        .expect("header range batch writes successfully");
    committed_hash
}

fn write_full_block_header_and_transactions(state: &ZebraDb, block: Arc<Block>) {
    let checkpoint_verified = CheckpointVerifiedBlock::from(block);
    let finalized =
        FinalizedBlock::from_checkpoint_verified(checkpoint_verified, Treestate::default());

    let mut batch = DiskWriteBatch::new();
    batch
        .prepare_block_header_and_transaction_data_batch(state, &finalized, true)
        .expect("full block header and transaction batch is valid");
    state.db.write(batch).expect("full block batch writes");
}

fn test_block_db_round_trip_with(
    network: &Network,
    block_test_cases: impl IntoIterator<Item = Block>,
) {
    let _init_guard = zebra_test::init();

    let state = ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        // The raw database accesses in this test create invalid database formats.
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    );

    // Check that each block round-trips to the database
    for original_block in block_test_cases.into_iter() {
        // First, check that the block round-trips without using the database
        let block_data = original_block
            .zcash_serialize_to_vec()
            .expect("serialization to vec never fails");
        let round_trip_block: Block = block_data
            .zcash_deserialize_into()
            .expect("deserialization of valid serialized block never fails");
        let round_trip_data = round_trip_block
            .zcash_serialize_to_vec()
            .expect("serialization to vec never fails");

        assert_eq!(
            original_block, round_trip_block,
            "test block structure must round-trip",
        );
        assert_eq!(
            block_data, round_trip_data,
            "test block data must round-trip",
        );

        // Now, use the database
        let original_block = Arc::new(original_block);
        let checkpoint_verified = if original_block.coinbase_height().is_some() {
            CheckpointVerifiedBlock::from(original_block.clone())
        } else {
            // Fake a zero height
            let hash = original_block.hash();
            let transaction_hashes: Arc<[_]> = original_block
                .transactions
                .iter()
                .map(|tx| tx.hash())
                .collect();
            let new_outputs =
                new_ordered_outputs_with_height(&original_block, Height(0), &transaction_hashes);

            CheckpointVerifiedBlock(SemanticallyVerifiedBlock {
                block: original_block.clone(),
                hash,
                height: Height(0),
                new_outputs,
                transaction_hashes,
                deferred_pool_balance_change: None,
            })
        };

        let dummy_treestate = Treestate::default();
        let finalized =
            FinalizedBlock::from_checkpoint_verified(checkpoint_verified, dummy_treestate);

        // Skip validation by writing the block directly to the database
        let mut batch = DiskWriteBatch::new();
        batch
            .prepare_block_header_and_transaction_data_batch(&state, &finalized, true)
            .expect("test block header and transaction batch is valid");
        state.db.write(batch).expect("block is valid for writing");

        // Now read it back from the state
        let stored_block = state
            .block(finalized.height.into())
            .expect("block was stored at height");

        if stored_block != original_block {
            error!(
                "
                detailed block mismatch report:
                original: {:?}\n\
                original data: {:?}\n\
                stored: {:?}\n\
                stored data: {:?}\n\
                ",
                original_block,
                hex::encode(original_block.zcash_serialize_to_vec().unwrap()),
                stored_block,
                hex::encode(stored_block.zcash_serialize_to_vec().unwrap()),
            );
        }

        assert_eq!(stored_block, original_block);
    }
}

/// Missing pruning metadata means this is an archive database from before pruning was added.
#[test]
fn missing_pruning_metadata_cf_is_archive_database() {
    let _init_guard = zebra_test::init();

    let column_families_without_pruning_metadata = STATE_COLUMN_FAMILIES_IN_CODE
        .iter()
        .filter(|cf_name| **cf_name != PRUNING_METADATA)
        .map(ToString::to_string);

    let state = ZebraDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Mainnet,
        true,
        column_families_without_pruning_metadata,
        false,
    );

    assert!(state.lowest_retained_height().is_none());
    assert!(!state.is_pruned());
}
