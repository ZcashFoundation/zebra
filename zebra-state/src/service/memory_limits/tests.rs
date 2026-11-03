//! The default state memory limits are today's behaviour, and the carried block charge.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use crate::{
    block_retained_charge, request::block_wire_and_retained_bytes, CheckpointVerifiedBlock, Config,
    ContextuallyVerifiedBlock, MemoryLimits, MinedBlockNotifications, Request, RocksDbMemoryLimits,
    SemanticallyVerifiedBlock, READ_PATH_COLUMN_FAMILIES,
};

/// The default limits open RocksDB with today's options, keep the usual reorg finalization rule,
/// and keep the 320 MiB orphan queue.
#[test]
fn default_memory_limits_are_todays_behaviour() {
    let _init_guard = zebra_test::init();

    assert_eq!(
        MemoryLimits::default(),
        MemoryLimits {
            rocksdb: RocksDbMemoryLimits::default(),
            non_finalized_resident_bytes: None,
            queued_blocks_bytes: 320 * 1024 * 1024,
        }
    );
}

/// `init` and `init_with_limits` with the default limits open the database with today's options.
///
/// A guard for D1, a node without a memory budget keeps today's behaviour: it passes with or
/// without the limits reaching the database, and (A)'s `default_limits_reproduce_todays_options`
/// checks the options those default limits apply.
#[tokio::test(flavor = "multi_thread")]
async fn default_limits_open_the_database_with_todays_options() {
    let _init_guard = zebra_test::init();

    let (_state, read_state, _, _) =
        crate::init(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    assert_eq!(
        read_state.db.memory_limits(),
        &RocksDbMemoryLimits::default()
    );

    let (_state, read_state, _, _) = crate::init_with_limits(
        Config::ephemeral(),
        &Network::Mainnet,
        Height::MAX,
        0,
        MemoryLimits::default(),
    )
    .await;
    assert_eq!(
        read_state.db.memory_limits(),
        &RocksDbMemoryLimits::default()
    );
}

/// `init_with_limits` opens the database with the RocksDB bounds in its limits.
#[tokio::test(flavor = "multi_thread")]
async fn init_with_limits_opens_the_database_with_its_rocksdb_limits() {
    let _init_guard = zebra_test::init();

    let rocksdb = RocksDbMemoryLimits {
        write_buffer_total: Some(64 * 1024 * 1024),
        block_cache_bytes: Some(32 * 1024 * 1024),
        charge_index_and_filter_cfs: READ_PATH_COLUMN_FAMILIES,
        ..RocksDbMemoryLimits::default()
    };
    let (_state, read_state, _, _) = crate::init_with_limits(
        Config::ephemeral(),
        &Network::Mainnet,
        Height::MAX,
        0,
        MemoryLimits {
            rocksdb,
            ..MemoryLimits::default()
        },
    )
    .await;

    assert_eq!(read_state.db.memory_limits(), &rocksdb);
}

/// The public charge is the block's share of the orphan queue and proposal cache charges, and
/// covers at least the block's wire bytes.
#[test]
fn block_retained_charge_is_the_shared_block_charge() {
    let _init_guard = zebra_test::init();

    for bytes in [
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
    ] {
        let block: Arc<Block> = bytes
            .zcash_deserialize_into()
            .expect("test vectors are valid");
        let prepared = SemanticallyVerifiedBlock::from(block.clone());
        let (wire_bytes, retained_bytes) =
            block_wire_and_retained_bytes(&block, &prepared.transaction_hashes);

        let charge = block_retained_charge(&block, &prepared.transaction_hashes);

        assert_eq!(charge, retained_bytes);
        assert!(charge > wire_bytes, "decoded metadata is charged too");
    }
}

/// Every constructor from a bare block leaves the charge for its consumer to compute, so no
/// constructor serializes the block.
#[test]
fn block_constructors_leave_the_memory_charge_unset() {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vectors are valid");

    let from_block = SemanticallyVerifiedBlock::from(block.clone());
    let with_hash = SemanticallyVerifiedBlock::with_hash(block.clone(), block.hash());
    let checkpoint = CheckpointVerifiedBlock::from(block);

    for prepared in [&from_block, &with_hash, &*checkpoint] {
        assert_eq!(prepared.retained_charge, None);
    }
}

/// Retention tokens carry their producer's charge into checkpoint and semantic records, so
/// downstream memory accounting does not serialize the block again.
#[test]
fn retention_tokens_carry_their_block_charge() {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vectors are valid");
    let charge = block_retained_charge(&block, &[]) + 1_000_000;
    let token = crate::BlockRetentionToken::new(()).with_retained_charge(charge);

    assert_eq!(crate::BlockRetentionToken::new(()).retained_charge(), None);
    assert_eq!(token.retained_charge(), Some(charge));

    let checkpoint = CheckpointVerifiedBlock::from(block.clone()).with_retention(token.clone());
    let semantic = SemanticallyVerifiedBlock::from(block).with_retention(token);

    assert_eq!(checkpoint.retained_charge, Some(charge));
    assert_eq!(semantic.retained_charge, Some(charge));
}

/// A contextually verified block keeps the charge its semantically verified block carried,
/// computes it once when none was carried, reuses it for its retained bytes, and carries it to
/// every semantically verified block built from it.
#[test]
fn contextual_blocks_carry_the_block_charge() {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vectors are valid");
    let mut prepared = SemanticallyVerifiedBlock::from(block.clone());
    let computed = block_retained_charge(&block, &prepared.transaction_hashes);

    let uncarried = ContextuallyVerifiedBlock::test_with_zero_spent_utxos(prepared.clone());
    assert_eq!(uncarried.retained_charge, computed);

    let carried_charge = computed + 1_000_000;
    prepared.retained_charge = Some(carried_charge);
    let carried = ContextuallyVerifiedBlock::test_with_zero_spent_utxos(prepared);
    assert_eq!(carried.retained_charge, carried_charge);
    assert_eq!(
        carried.retained_bytes(),
        carried_charge + carried.contextual_data_retained_bytes(),
        "the retained bytes reuse the carried charge",
    );
    assert_eq!(
        carried.retained_bytes(),
        uncarried.retained_bytes() + 1_000_000
    );

    assert_eq!(
        SemanticallyVerifiedBlock::from(carried.clone()).retained_charge,
        Some(carried_charge),
    );
    let (committed, _commits) = tokio::sync::mpsc::channel(1);
    let (validated, _bodies) = tokio::sync::watch::channel(None);
    let Request::CommitMinedBlock {
        block, contextual, ..
    } = carried.into_mined_request(MinedBlockNotifications::new(committed, validated))
    else {
        unreachable!("into_mined_request always creates a mined commit");
    };
    assert_eq!(block.retained_charge, Some(carried_charge));
    assert_eq!(
        contextual
            .expect("a mined commit keeps its contextual record")
            .retained_charge,
        carried_charge,
    );
}

/// The carried charge is derived from the block, so it takes no part in the equality of the
/// non-finalized records built from it.
#[test]
fn contextual_block_equality_ignores_the_block_charge() {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vectors are valid");
    let mut prepared = SemanticallyVerifiedBlock::from(block);
    let computed = ContextuallyVerifiedBlock::test_with_zero_spent_utxos(prepared.clone());
    prepared.retained_charge = Some(computed.retained_charge + 1_000_000);
    let carried = ContextuallyVerifiedBlock::test_with_zero_spent_utxos(prepared);

    assert_ne!(carried.retained_charge, computed.retained_charge);
    assert_eq!(carried, computed);

    let other: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("test vectors are valid");
    assert_ne!(
        ContextuallyVerifiedBlock::test_with_zero_spent_utxos(other),
        computed,
        "records of different blocks still differ",
    );
}
