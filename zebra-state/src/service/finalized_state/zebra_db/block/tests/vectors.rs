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

use std::{iter, sync::Arc};

use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::{
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
            large_single_transaction_block_many_outputs,
        },
        Block, Height,
    },
    parameters::Network::{self, *},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent::new_ordered_outputs_with_height,
};
use zebra_test::vectors::{MAINNET_BLOCKS, TESTNET_BLOCKS};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    request::{FinalizedBlock, Treestate},
    service::finalized_state::{
        disk_db::{DiskWriteBatch, WriteDisk},
        disk_format::TransactionLocation,
        ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    CheckpointVerifiedBlock, Config, SemanticallyVerifiedBlock,
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
    )
    .expect("opening an ephemeral database should succeed");

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

            CheckpointVerifiedBlock::from(SemanticallyVerifiedBlock {
                block: original_block.clone(),
                hash,
                height: Height(0),
                new_outputs,
                transaction_hashes,
                received_time: None,
            })
        };

        let dummy_treestate = Treestate::default();
        let finalized = FinalizedBlock::from_checkpoint_verified(
            checkpoint_verified,
            dummy_treestate,
            DeferredPoolBalanceChange::zero(),
        );

        // Skip validation by writing the block directly to the database
        let mut batch = DiskWriteBatch::new();
        batch.prepare_block_header_and_transaction_data_batch(&state.db, &finalized);

        // Compare the entire ordered batch with the serial typed-serialization path.
        let mut serial_batch = DiskWriteBatch::new();
        let cf = |name| {
            state
                .db
                .cf_handle(name)
                .expect("the test database has this column")
        };
        serial_batch.zs_insert(
            &cf("block_header_by_height"),
            finalized.height,
            &original_block.header,
        );
        serial_batch.zs_insert(&cf("hash_by_height"), finalized.height, finalized.hash);
        serial_batch.zs_insert(&cf("height_by_hash"), finalized.hash, finalized.height);
        for (index, (tx, hash)) in original_block
            .transactions
            .iter()
            .zip(finalized.transaction_hashes.iter())
            .enumerate()
        {
            let location = TransactionLocation::from_usize(finalized.height, index);
            serial_batch.zs_insert(&cf("tx_by_loc"), location, tx);
            serial_batch.zs_insert(&cf("hash_by_tx_loc"), location, hash);
            serial_batch.zs_insert(&cf("tx_loc_by_hash"), hash, location);
        }
        assert_eq!(
            batch, serial_batch,
            "parallel serialization must preserve batch bytes"
        );
        state.db.write(batch).expect("block is valid for writing");

        // Now read it back from the state
        let stored_block = state
            .block(finalized.height.into())
            .expect("stored block body is complete")
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
        for transaction in &original_block.transactions {
            assert_eq!(
                state.transaction_version(transaction.hash()),
                Some(transaction.version()),
                "the stored header must return the version without the Overwinter flag",
            );
        }
    }
}

/// Stores just the block columns, leaving consensus validation to the existing state tests.
fn complete_body_test_db(config: &Config) -> (ZebraDb, Arc<Block>) {
    let db = ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    let block: Arc<Block> = MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .find(|block| block.transactions.len() >= 3)
        .unwrap();
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
    (db, block)
}

#[test]
fn incomplete_block_bodies_are_not_unknown_or_partial() {
    use crate::{BlockReadError, HashOrHeight};

    let _init_guard = zebra_test::init();
    let (db, block) = complete_body_test_db(&Config::ephemeral());
    let height = block.coinbase_height().unwrap();
    let txs = db.cf_handle("tx_by_loc").unwrap();
    let expected = BlockReadError::CorruptBlock { height };

    for query in [
        HashOrHeight::Height(height.next().unwrap()),
        HashOrHeight::Hash(zebra_chain::block::Hash([0xff; 32])),
    ] {
        assert!(db.block(query).unwrap().is_none());
        assert!(db.raw_block(query).unwrap().is_none());
        assert!(db.block_and_size(query).unwrap().is_none());
    }
    assert!(db
        .transaction(zebra_chain::transaction::Hash([0xff; 32]))
        .unwrap()
        .is_none());

    // Include a missing tail: checking only that the remaining rows are contiguous is insufficient.
    for missing in [None, Some(0), Some(1), Some(block.transactions.len() - 1)] {
        let mut batch = DiskWriteBatch::new();
        for (index, tx) in block.transactions.iter().enumerate() {
            let location = TransactionLocation::from_usize(height, index);
            batch.zs_insert(&txs, location, tx);
            if missing.is_none_or(|missing| missing == index) {
                batch.zs_delete(&txs, location);
            }
        }
        db.write_batch(batch).unwrap();
        for query in [
            HashOrHeight::Height(height),
            HashOrHeight::Hash(block.hash()),
        ] {
            assert_eq!(db.block(query).unwrap_err(), expected);
            assert_eq!(db.raw_block(query).unwrap_err(), expected);
            assert_eq!(db.block_and_size(query).unwrap_err(), expected);
            assert!(db.block_header(query).is_some());
        }
    }
    let mut batch = DiskWriteBatch::new();
    for (index, tx) in block.transactions.iter().enumerate() {
        batch.zs_insert(&txs, TransactionLocation::from_usize(height, index), tx);
    }
    db.write_batch(batch).unwrap();
    for (index, tx) in block.transactions.iter().enumerate() {
        assert_eq!(db.transaction(tx.hash()).unwrap().unwrap().0, tx.clone());
        let mut batch = DiskWriteBatch::new();
        batch.zs_delete(&txs, TransactionLocation::from_usize(height, index));
        db.write_batch(batch).unwrap();
        assert_eq!(db.transaction(tx.hash()).unwrap_err(), expected);
    }

    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(&db.cf_handle("block_header_by_height").unwrap(), height);
    db.write_batch(batch).unwrap();
    for query in [
        HashOrHeight::Height(height),
        HashOrHeight::Hash(block.hash()),
    ] {
        assert_eq!(
            db.block(query).unwrap_err(),
            BlockReadError::CorruptBlock { height }
        );
    }
    assert_eq!(
        db.transaction(block.transactions[0].hash()).unwrap_err(),
        expected
    );
}

#[test]
fn concurrent_block_body_deletion_never_returns_partial_data() {
    use std::sync::Barrier;

    use crate::BlockReadError;

    let _init_guard = zebra_test::init();
    let (db, block) = complete_body_test_db(&Config::ephemeral());
    let height = block.coinbase_height().unwrap();
    let expected_size = block.zcash_serialized_size();
    let barrier = Barrier::new(2);
    let unavailable = BlockReadError::CorruptBlock { height };

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let txs = db.cf_handle("tx_by_loc").unwrap();
            barrier.wait();
            for round in 0..64 {
                let mut batch = DiskWriteBatch::new();
                if round % 2 == 0 {
                    batch.zs_delete_range(
                        &txs,
                        TransactionLocation::min_for_height(height),
                        TransactionLocation::min_for_height(height.next().unwrap()),
                    );
                } else {
                    for (index, tx) in block.transactions.iter().enumerate() {
                        batch.zs_insert(&txs, TransactionLocation::from_usize(height, index), tx);
                    }
                }
                db.write_batch(batch).unwrap();
            }
        });
        barrier.wait();
        for _ in 0..64 {
            match db.block(height.into()) {
                Ok(Some(actual)) => assert_eq!(actual, block),
                Err(error) => assert_eq!(error, unavailable),
                other => panic!("known block must remain known: {other:?}"),
            }
            match db.raw_block(height.into()) {
                Ok(Some((header, transactions))) => {
                    assert_eq!(
                        header.raw_bytes(),
                        &block.header.zcash_serialize_to_vec().unwrap()
                    );
                    assert_eq!(transactions.len(), block.transactions.len());
                    for (raw, tx) in transactions.iter().zip(&block.transactions) {
                        assert_eq!(raw.raw_bytes(), &tx.zcash_serialize_to_vec().unwrap());
                    }
                }
                Err(error) => assert_eq!(error, unavailable),
                other => panic!("known block must remain known: {other:?}"),
            }
            match db.block_and_size(height.into()) {
                Ok(Some((actual, size))) => {
                    assert_eq!(actual, block);
                    assert_eq!(size, expected_size);
                }
                Err(error) => assert_eq!(error, unavailable),
                other => panic!("known block must remain known: {other:?}"),
            }
        }
    });
}

#[test]
fn secondary_block_reads_wait_for_complete_cross_column_view() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let (primary, block) = complete_body_test_db(&config);
    let height = block.coinbase_height().unwrap();
    primary.db.flush().unwrap();
    let secondary = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        true,
    )
    .unwrap();
    assert_eq!(secondary.block(height.into()).unwrap(), Some(block.clone()));
    assert_eq!(
        secondary
            .transaction(block.transactions[0].hash())
            .unwrap()
            .unwrap()
            .0,
        block.transactions[0]
    );
    assert_eq!(
        secondary.block_and_size(height.into()).unwrap(),
        Some((block.clone(), block.zcash_serialized_size()))
    );

    let headers = primary.cf_handle("block_header_by_height").unwrap();
    let txs = primary.cf_handle("tx_by_loc").unwrap();
    let hashes = primary.cf_handle("hash_by_tx_loc").unwrap();
    let complete_batch = || {
        let mut batch = DiskWriteBatch::new();
        batch.zs_insert(&headers, height, &block.header);
        for (index, tx) in block.transactions.iter().enumerate() {
            let location = TransactionLocation::from_usize(height, index);
            batch.zs_insert(&txs, location, tx);
            batch.zs_insert(&hashes, location, tx.hash());
        }
        batch
    };
    let last = TransactionLocation::from_usize(height, block.transactions.len() - 1);
    for lagging_column in 0..6 {
        let mut batch = complete_batch();
        // Expose columns out of step, as a secondary can while applying a primary write batch.
        match lagging_column {
            0 => batch.zs_delete(&headers, height),
            1 => batch.zs_delete(&txs, TransactionLocation::min_for_height(height)),
            2 => batch.zs_delete(&txs, last),
            3 => batch.zs_delete_range(
                &hashes,
                TransactionLocation::min_for_height(height),
                TransactionLocation::min_for_height(height.next().unwrap()),
            ),
            4 => batch.zs_delete(&hashes, last),
            _ => {
                // Both columns can expose the same shortened prefix during WAL replay.
                for cf in [&txs, &hashes] {
                    batch.zs_delete_range(
                        cf,
                        TransactionLocation::from_usize(height, 1),
                        TransactionLocation::min_for_height(height.next().unwrap()),
                    );
                }
            }
        }
        primary.write_batch(batch).unwrap();
        primary.db.flush().unwrap();
        secondary.try_catch_up_with_primary().unwrap();
        if lagging_column < 5 {
            assert!(primary.block(height.into()).is_err());
        }
        for query in [height.into(), block.hash().into()] {
            assert!(secondary.block(query).unwrap().is_none());
            assert!(secondary.raw_block(query).unwrap().is_none());
            assert!(secondary.block_and_size(query).unwrap().is_none());
        }
        if lagging_column <= 2 {
            let tx = if lagging_column == 2 {
                block.transactions.last().unwrap()
            } else {
                &block.transactions[0]
            };
            assert!(secondary.transaction(tx.hash()).unwrap().is_none());
            assert_eq!(
                primary.transaction(tx.hash()).unwrap_err(),
                crate::BlockReadError::CorruptBlock { height }
            );
        }
    }
    primary.write_batch(complete_batch()).unwrap();
    primary.db.flush().unwrap();
    secondary.try_catch_up_with_primary().unwrap();
    assert_eq!(secondary.block(height.into()).unwrap(), Some(block));
}

#[test]
fn block_read_racing_header_commit() {
    let _init_guard = zebra_test::init();
    let (db, block) = complete_body_test_db(&Config::ephemeral());
    let height = block.coinbase_height().unwrap();

    for reader in 0..3 {
        let headers = db.cf_handle("block_header_by_height").unwrap();
        let heights = db.cf_handle("hash_by_height").unwrap();
        let mut hidden = DiskWriteBatch::new();
        hidden.zs_delete(&headers, height);
        hidden.zs_delete(&heights, height);
        db.write_batch(hidden).unwrap();
        assert!(!db.contains_height(height));

        let mut commit = DiskWriteBatch::new();
        commit.zs_insert(&headers, height, &block.header);
        commit.zs_insert(&heights, height, block.hash());
        let writer = db.clone();
        super::AFTER_MISSING_HEADER.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || writer.write_batch(commit).unwrap()));
        });

        let found = match reader {
            0 => db.block(height.into()).map(|block| block.is_some()),
            1 => db.raw_block(height.into()).map(|block| block.is_some()),
            _ => db
                .block_and_size(height.into())
                .map(|block| block.is_some()),
        };
        assert_eq!(found, Ok(true), "a concurrent commit must not look corrupt");
        super::AFTER_MISSING_HEADER.with(|hook| assert!(hook.borrow().is_none()));
    }
}

#[test]
fn block_read_stops_at_height_boundary() {
    use rocksdb::{
        perf::{set_perf_stats, PerfContext, PerfMetric, PerfStatsLevel},
        CompactOptions, WaitForCompactOptions,
    };

    use crate::IntoDisk;

    let _init_guard = zebra_test::init();
    let (db, block) = complete_body_test_db(&Config::ephemeral());
    let height = block.coinbase_height().unwrap();
    let txs = db.cf_handle("tx_by_loc").unwrap();
    let mut options = CompactOptions::default();
    options.set_change_level(true);
    options.set_target_level(1);
    let mut settled = WaitForCompactOptions::default();
    settled.set_timeout(10_000_000);
    // Background compaction may move files to another level; let it finish before measuring.
    let compact = |height| {
        db.db.compact_range_cf_opt(
            &txs,
            Some(TransactionLocation::min_for_height(height).as_bytes()),
            Some(TransactionLocation::max_for_height(height).as_bytes()),
            &options,
        );
        db.db.wait_for_compact(&settled).unwrap();
    };
    db.db.flush_cf(&txs).unwrap();
    compact(height);
    db.raw_block(height.into()).unwrap().unwrap();

    set_perf_stats(PerfStatsLevel::EnableCount);
    let mut counters = PerfContext::default();
    counters.reset();
    db.raw_block(height.into()).unwrap().unwrap();
    let isolated_block_reads = counters.metric(PerfMetric::BlockReadCount)
        + counters.metric(PerfMetric::BlockCacheHitCount);

    // Put the next height in a separate SST. Finishing this body must not visit that row.
    let mut batch = DiskWriteBatch::new();
    batch.zs_insert(
        &txs,
        TransactionLocation::min_for_height(height.next().unwrap()),
        &block.transactions[0],
    );
    db.write_batch(batch).unwrap();
    db.db.flush_cf(&txs).unwrap();
    compact(height.next().unwrap());
    let last = TransactionLocation::max_for_height(height).as_bytes();
    let next = TransactionLocation::min_for_height(height.next().unwrap()).as_bytes();
    assert!(
        db.db.live_files().unwrap().iter().all(|file| {
            file.column_family_name != "tx_by_loc"
                || (file.level > 0
                    && (file.end_key.as_deref() < Some(&next[..])
                        || file.start_key.as_deref() > Some(&last[..])))
        }),
        "adjacent heights must occupy separate sorted-level files"
    );
    counters.reset();
    db.raw_block(height.into()).unwrap().unwrap();
    let adjacent_block_reads = counters.metric(PerfMetric::BlockReadCount)
        + counters.metric(PerfMetric::BlockCacheHitCount);
    set_perf_stats(PerfStatsLevel::Disable);

    assert!(
        isolated_block_reads > 0,
        "block read accounting must be enabled"
    );
    assert_eq!(
        adjacent_block_reads, isolated_block_reads,
        "a block read must not advance into the next height"
    );
}
