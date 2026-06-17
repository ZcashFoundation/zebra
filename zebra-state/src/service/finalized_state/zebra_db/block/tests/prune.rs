//! Tests for pruned storage mode.
//!
//! These tests cover:
//! - the pure prune-range arithmetic ([`prune_height_range_inner`]),
//! - the deletion mechanism ([`DiskWriteBatch::prepare_prune_batch`]): historical
//!   transaction data is removed while consensus-critical state survives,
//! - the one-way marker: a pruned database cannot be reopened in archive mode,
//! - configuration validation of the retention floor.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::{
    block::{Block, Height},
    parameters::Network::{self, Mainnet},
    serialization::ZcashDeserializeInto,
    transparent,
};

use crate::{
    config::StorageMode,
    constants::{MAX_BLOCK_REORG_HEIGHT, MAX_PRUNE_HEIGHTS_PER_COMMIT, MIN_PRUNING_RETENTION},
    request::{FinalizableBlock, Treestate},
    rollback_finalized_state,
    service::finalized_state::{disk_db::DiskWriteBatch, FinalizedState},
    Config, ContextuallyVerifiedBlock, PruningConfig, RollbackFinalizedStateError,
    RollbackFinalizedStateOptions, SemanticallyVerifiedBlock,
};

use super::super::{prune_height_range_inner, should_log_prune_progress};

/// The number of leading blocks committed by the database-backed prune tests.
const TEST_BLOCKS: u32 = 9;

/// Opens a fresh finalized state and commits blocks `0..=TEST_BLOCKS` for `network`.
fn new_state_with_blocks(config: &Config, network: &Network) -> FinalizedState {
    let mut state = FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    let blocks = network.blockchain_map();
    for height in 0..=TEST_BLOCKS {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        state
            .commit_finalized_direct(block.into(), None, "prune tests")
            .expect("test block is valid");
    }

    state
}

/// Opens a fresh finalized state with a checkpoint retention floor and commits
/// blocks `0..=TEST_BLOCKS` for `network`.
fn new_state_with_checkpoint_retention(
    config: &Config,
    network: &Network,
    max_checkpoint_height: Height,
) -> FinalizedState {
    let mut state = FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .with_checkpoint_raw_tx_retention(max_checkpoint_height, config);

    let blocks = network.blockchain_map();
    for height in 0..=TEST_BLOCKS {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        state
            .commit_finalized_direct(block.into(), None, "checkpoint retention tests")
            .expect("test block is valid");
    }

    state
}

/// Opens a fresh finalized state without validating `config.storage_mode`, then
/// configures checkpoint retention.
///
/// This lets short vector-based tests exercise the commit-time handoff between
/// checkpoint raw transaction skipping and online pruning with a tiny retention.
fn new_unvalidated_state_with_checkpoint_retention(
    config: &Config,
    network: &Network,
    max_checkpoint_height: Height,
) -> FinalizedState {
    FinalizedState::new_with_debug_without_storage_validation(
        config,
        network,
        false,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .with_checkpoint_raw_tx_retention(max_checkpoint_height, config)
}

/// Returns a pruned-mode config with a valid mainnet retention window.
fn pruned_config() -> Config {
    Config {
        storage_mode: StorageMode::Pruned(PruningConfig {
            tx_retention: MIN_PRUNING_RETENTION,
        }),
        ..Config::ephemeral()
    }
}

/// Returns the coinbase transaction hash of the block at `height`.
fn coinbase_tx_hash(network: &Network, height: u32) -> zebra_chain::transaction::Hash {
    let block: Arc<Block> = network
        .blockchain_map()
        .get(&height)
        .expect("block height has test data")
        .zcash_deserialize_into()
        .expect("test data deserializes");

    block.transactions[0].hash()
}

#[test]
fn checkpoint_retention_hands_off_to_online_pruning_at_floor() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let tx_retention = 5;
    let config = Config {
        storage_mode: StorageMode::Pruned(PruningConfig { tx_retention }),
        ..Config::ephemeral()
    };
    let checkpoint_lowest_retained = Height(3);
    let max_checkpoint_height = Height(tx_retention + checkpoint_lowest_retained.0 - 1);
    let mut state =
        new_unvalidated_state_with_checkpoint_retention(&config, &network, max_checkpoint_height);
    let blocks = network.blockchain_map();

    for height in 0..=max_checkpoint_height.0 {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        state
            .commit_finalized_direct(block.into(), None, "checkpoint handoff tests")
            .expect("test block is valid");
    }

    assert_eq!(
        state.db.lowest_retained_height(),
        Some(checkpoint_lowest_retained),
        "checkpoint skipping advances the marker to the retained floor"
    );

    for height in 1..checkpoint_lowest_retained.0 {
        assert!(
            state
                .db
                .transaction(coinbase_tx_hash(&network, height))
                .is_none(),
            "raw transaction is skipped below the checkpoint retention floor"
        );
    }
    assert!(
        state
            .db
            .transaction(coinbase_tx_hash(&network, checkpoint_lowest_retained.0))
            .is_some(),
        "raw transaction is retained at the checkpoint retention floor before handoff"
    );

    let handoff_tip = (max_checkpoint_height + 1).expect("max checkpoint height plus one is valid");
    let block: Arc<Block> = blocks
        .get(&handoff_tip.0)
        .expect("block height has test data")
        .zcash_deserialize_into()
        .expect("test data deserializes");

    state
        .commit_finalized_direct(block.into(), None, "checkpoint handoff tests")
        .expect("handoff block is valid");

    let online_prune_until =
        (checkpoint_lowest_retained + 1).expect("checkpoint retention floor plus one is valid");
    assert_eq!(
        state.db.lowest_retained_height(),
        Some(online_prune_until),
        "online pruning resumes exactly at the checkpoint retention floor"
    );
    assert!(
        state
            .db
            .transaction(coinbase_tx_hash(&network, checkpoint_lowest_retained.0))
            .is_none(),
        "online pruning deletes the first retained-floor height after the checkpoint target"
    );
    assert!(
        state
            .db
            .transaction(coinbase_tx_hash(&network, online_prune_until.0))
            .is_some(),
        "the next height remains retained, so there is no pruning gap"
    );
    assert_eq!(
        Some(checkpoint_lowest_retained),
        online_prune_until - 1,
        "skipped heights end immediately before the online-pruned range starts"
    );
}

#[test]
fn prune_height_range_arithmetic() {
    // Nothing to prune until the tip is `retention` blocks past genesis.
    assert_eq!(prune_height_range_inner(100, 5000, None), None, "underflow");
    assert_eq!(
        prune_height_range_inner(5000, 5000, None),
        None,
        "tip == retention: only genesis would be eligible, which is never pruned"
    );

    // First online prune starts at the current retention boundary, preserving
    // older archive history that was synced before pruning was enabled.
    assert_eq!(
        prune_height_range_inner(6, 5, None),
        Some((1, 2)),
        "first prunable height is 1, never genesis"
    );
    assert_eq!(
        prune_height_range_inner(10_000, 5000, None),
        Some((5000, 5001)),
        "first online prune starts at the retention boundary"
    );

    // Steady state: each new block makes exactly one new height prunable.
    assert_eq!(
        prune_height_range_inner(7, 5, Some(2)),
        Some((2, 3)),
        "resumes from the lowest retained height"
    );

    // Nothing new to prune yet (already pruned up to the current eligible height).
    assert_eq!(prune_height_range_inner(6, 5, Some(2)), None, "nothing new");

    // Backlog drain after an existing marker is bounded to
    // MAX_PRUNE_HEIGHTS_PER_COMMIT heights per commit.
    let (from, until) =
        prune_height_range_inner(100_000, 5000, Some(1)).expect("backlog should prune");
    assert_eq!(from, 1);
    assert_eq!(
        until - from,
        MAX_PRUNE_HEIGHTS_PER_COMMIT,
        "per-commit work is capped"
    );
}

#[test]
fn checkpoint_raw_transaction_prune_range_bounds_work() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let state = new_state_with_blocks(&Config::ephemeral(), &network);

    assert_eq!(
        state.db.checkpoint_raw_transaction_prune_range(Height(1)),
        None,
        "genesis is never included in checkpoint archive-backlog pruning"
    );

    assert_eq!(
        state
            .db
            .checkpoint_raw_transaction_prune_range(Height(TEST_BLOCKS + 1)),
        Some((Height(1), Height(TEST_BLOCKS + 1))),
        "checkpoint backlog pruning starts at height 1 when there is no marker"
    );

    assert_eq!(
        state
            .db
            .checkpoint_raw_transaction_prune_range(Height(MAX_PRUNE_HEIGHTS_PER_COMMIT + 2)),
        Some((Height(1), Height(MAX_PRUNE_HEIGHTS_PER_COMMIT + 1))),
        "checkpoint backlog pruning is bounded per commit"
    );

    let mut batch = DiskWriteBatch::new();
    batch.prepare_prune_batch(&state.db, Height(1), Height(4));
    state.db.write_batch(batch).expect("prune batch writes");

    assert_eq!(
        state.db.checkpoint_raw_transaction_prune_range(Height(8)),
        Some((Height(4), Height(8))),
        "checkpoint backlog pruning resumes from the existing marker"
    );

    let mut batch = DiskWriteBatch::new();
    batch.prepare_prune_batch(&state.db, Height(4), Height(8));
    state.db.write_batch(batch).expect("prune batch writes");

    assert_eq!(
        state.db.checkpoint_raw_transaction_prune_range(Height(8)),
        None,
        "no checkpoint backlog prune is needed when the marker reaches the skipped height"
    );
}

#[test]
fn prune_progress_logging_is_chunked() {
    assert!(
        should_log_prune_progress(false, Height(5001), Height(1), Height(2)),
        "first prune is logged so operators can see destructive pruning started"
    );

    assert!(
        should_log_prune_progress(
            true,
            Height(5001),
            Height(1),
            Height(1 + MAX_PRUNE_HEIGHTS_PER_COMMIT),
        ),
        "full backlog chunks are logged"
    );

    assert!(
        should_log_prune_progress(true, Height(5100), Height(99), Height(100)),
        "steady-state pruning logs on 100-block tip boundaries"
    );

    assert!(
        !should_log_prune_progress(true, Height(5101), Height(100), Height(101)),
        "steady-state pruning does not log every block"
    );
}

#[test]
fn checkpoint_retention_skips_old_raw_transactions_in_pruned_mode() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let config = pruned_config();
    let checkpoint_lowest_retained = Height(3);
    let max_checkpoint_height = Height(MIN_PRUNING_RETENTION + checkpoint_lowest_retained.0 - 1);

    let state = new_state_with_checkpoint_retention(&config, &network, max_checkpoint_height);

    assert!(
        state
            .db
            .transaction(coinbase_tx_hash(&network, 0))
            .is_some(),
        "genesis raw transaction is always retained"
    );

    for height in 1..checkpoint_lowest_retained.0 {
        let tx_hash = coinbase_tx_hash(&network, height);

        assert!(
            state.db.transaction(tx_hash).is_none(),
            "checkpoint raw transaction is skipped below the checkpoint retention floor"
        );
        assert_eq!(
            state.db.transactions_by_height(Height(height)).count(),
            0,
            "tx_by_loc has no raw transactions below the checkpoint retention floor"
        );
        assert!(
            state.db.transaction_location(tx_hash).is_some(),
            "transaction location index is retained for skipped checkpoint transaction"
        );
        assert!(
            state.db.block_header(Height(height).into()).is_some(),
            "block header is retained for skipped checkpoint transaction"
        );
        assert!(
            state.db.block(Height(height).into()).is_none(),
            "block reconstruction fails cleanly when raw checkpoint transactions are skipped"
        );
    }

    for height in checkpoint_lowest_retained.0..=TEST_BLOCKS {
        let tx_hash = coinbase_tx_hash(&network, height);

        assert!(
            state.db.transaction(tx_hash).is_some(),
            "checkpoint raw transaction is retained at or above the checkpoint retention floor"
        );
        assert!(
            state.db.block(Height(height).into()).is_some(),
            "retained checkpoint-window block can be reconstructed"
        );
    }

    assert_eq!(
        state.db.lowest_retained_height(),
        Some(checkpoint_lowest_retained),
        "skipped checkpoint raw transactions advance the pruning marker atomically"
    );
}

#[test]
fn archive_to_pruned_checkpoint_sync_drains_archive_raw_transactions_before_skipping() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let dir = tempfile::tempdir().expect("temp dir is created");
    let archive_config = Config {
        cache_dir: dir.path().to_path_buf(),
        ephemeral: false,
        ..Config::ephemeral()
    };
    let blocks = network.blockchain_map();

    let mut archive_state = FinalizedState::new(
        &archive_config,
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    for height in 0..TEST_BLOCKS {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        archive_state
            .commit_finalized_direct(block.into(), None, "archive phase")
            .expect("archive block is valid");
    }

    assert_eq!(
        archive_state.db.lowest_retained_height(),
        None,
        "archive phase has no pruning marker"
    );
    assert!(
        archive_state
            .db
            .transaction(coinbase_tx_hash(&network, TEST_BLOCKS - 1))
            .is_some(),
        "archive phase stores raw transactions below the future checkpoint floor"
    );
    std::mem::drop(archive_state);

    let tx_retention = 5;
    let checkpoint_lowest_retained = Height(TEST_BLOCKS + 1);
    let max_checkpoint_height = Height(tx_retention + checkpoint_lowest_retained.0 - 1);
    let pruned_config = Config {
        cache_dir: dir.path().to_path_buf(),
        ephemeral: false,
        storage_mode: StorageMode::Pruned(PruningConfig { tx_retention }),
        ..Config::ephemeral()
    };
    let mut pruned_state = new_unvalidated_state_with_checkpoint_retention(
        &pruned_config,
        &network,
        max_checkpoint_height,
    );

    let block: Arc<Block> = blocks
        .get(&TEST_BLOCKS)
        .expect("block height has test data")
        .zcash_deserialize_into()
        .expect("test data deserializes");

    pruned_state
        .commit_finalized_direct(block.into(), None, "archive to pruned checkpoint")
        .expect("checkpoint block is valid");

    assert_eq!(
        pruned_state.db.lowest_retained_height(),
        Some(checkpoint_lowest_retained),
        "archive backlog is pruned up to the checkpoint retention floor"
    );

    for height in 1..checkpoint_lowest_retained.0 {
        assert!(
            pruned_state
                .db
                .transaction(coinbase_tx_hash(&network, height))
                .is_none(),
            "archive raw transaction data is pruned at height {height}"
        );
        assert!(
            pruned_state
                .db
                .transaction_location(coinbase_tx_hash(&network, height))
                .is_some(),
            "transaction location index is retained at height {height}"
        );
    }
}

/// Reopening a pruned database recomputes the archive raw transaction backlog
/// flag: it is detected on the first pruned open, cleared once the backlog is
/// drained, and stays cleared across a subsequent restart.
#[test]
fn archive_backlog_flag_is_recomputed_when_reopening_a_pruned_database() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let dir = tempfile::tempdir().expect("temp dir is created");
    let archive_config = Config {
        cache_dir: dir.path().to_path_buf(),
        ephemeral: false,
        ..Config::ephemeral()
    };
    let blocks = network.blockchain_map();

    // Archive phase: store raw transactions for every block below the future
    // checkpoint retention floor.
    let mut archive_state = FinalizedState::new(
        &archive_config,
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    for height in 0..TEST_BLOCKS {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        archive_state
            .commit_finalized_direct(block.into(), None, "archive phase")
            .expect("archive block is valid");
    }
    std::mem::drop(archive_state);

    let tx_retention = 5;
    let checkpoint_lowest_retained = Height(TEST_BLOCKS + 1);
    let max_checkpoint_height = Height(tx_retention + checkpoint_lowest_retained.0 - 1);
    let pruned_config = Config {
        cache_dir: dir.path().to_path_buf(),
        ephemeral: false,
        storage_mode: StorageMode::Pruned(PruningConfig { tx_retention }),
        ..Config::ephemeral()
    };

    // First pruned open: the archive backlog below the floor must be detected.
    let mut pruned_state = new_unvalidated_state_with_checkpoint_retention(
        &pruned_config,
        &network,
        max_checkpoint_height,
    );
    assert!(
        pruned_state.has_checkpoint_raw_tx_archive_backlog(),
        "archive backlog is detected when first reopening as pruned"
    );

    // The small backlog drains in a single commit, clearing the flag.
    let block: Arc<Block> = blocks
        .get(&TEST_BLOCKS)
        .expect("block height has test data")
        .zcash_deserialize_into()
        .expect("test data deserializes");
    pruned_state
        .commit_finalized_direct(block.into(), None, "archive to pruned checkpoint")
        .expect("checkpoint block is valid");
    assert_eq!(
        pruned_state.db.lowest_retained_height(),
        Some(checkpoint_lowest_retained),
        "archive backlog is pruned up to the checkpoint retention floor"
    );
    assert!(
        !pruned_state.has_checkpoint_raw_tx_archive_backlog(),
        "flag is cleared once the archive backlog is drained"
    );
    std::mem::drop(pruned_state);

    // Reopen again: the drained database must recompute the flag to `false`, so
    // it does not attempt to re-drain a backlog that no longer exists.
    let reopened = new_unvalidated_state_with_checkpoint_retention(
        &pruned_config,
        &network,
        max_checkpoint_height,
    );
    assert!(
        !reopened.has_checkpoint_raw_tx_archive_backlog(),
        "a drained database recomputes no archive backlog on reopen"
    );
    assert_eq!(
        reopened.db.lowest_retained_height(),
        Some(checkpoint_lowest_retained),
        "the pruning marker is preserved across the reopen"
    );
}

#[test]
fn archive_mode_keeps_checkpoint_raw_transactions_below_checkpoint_retention_floor() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let config = Config::ephemeral();
    let max_checkpoint_height = Height(MIN_PRUNING_RETENTION + 2);

    let state = new_state_with_checkpoint_retention(&config, &network, max_checkpoint_height);

    for height in 1..=TEST_BLOCKS {
        let tx_hash = coinbase_tx_hash(&network, height);

        assert!(
            state.db.transaction(tx_hash).is_some(),
            "archive mode keeps checkpoint raw transaction data"
        );
        assert!(
            state.db.block(Height(height).into()).is_some(),
            "archive mode reconstructs checkpoint blocks"
        );
    }

    assert_eq!(
        state.db.lowest_retained_height(),
        None,
        "archive mode does not write a pruning marker"
    );
}

#[test]
fn contextual_commits_keep_raw_transactions_below_checkpoint_retention_floor() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let config = pruned_config();
    let max_checkpoint_height = Height(MIN_PRUNING_RETENTION + 2);
    let mut state = FinalizedState::new(
        &config,
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .with_checkpoint_raw_tx_retention(max_checkpoint_height, &config);
    let blocks = network.blockchain_map();

    let genesis: Arc<Block> = blocks
        .get(&0)
        .expect("genesis test data exists")
        .zcash_deserialize_into()
        .expect("genesis test data deserializes");
    state
        .commit_finalized_direct(genesis.into(), None, "contextual retention tests")
        .expect("genesis block is valid");

    let block: Arc<Block> = blocks
        .get(&1)
        .expect("block height has test data")
        .zcash_deserialize_into()
        .expect("test data deserializes");
    let contextually_verified = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        SemanticallyVerifiedBlock::from(block.clone()),
        HashMap::new(),
    )
    .expect("block has no external spent outputs in this test");
    let finalizable = FinalizableBlock::new(contextually_verified, Treestate::default());

    state
        .commit_finalized_direct(finalizable, None, "contextual retention tests")
        .expect("contextual block is valid");

    assert!(
        state
            .db
            .transaction(coinbase_tx_hash(&network, 1))
            .is_some(),
        "contextual finalized commits keep raw transaction data even below the checkpoint floor"
    );
    assert_eq!(
        state.db.lowest_retained_height(),
        None,
        "contextual commit below checkpoint floor does not advance pruning marker"
    );
}

#[test]
fn rollback_reports_missing_block_when_checkpoint_raw_transactions_were_skipped() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    let dir = tempfile::tempdir().expect("temp dir is created");
    let config = Config {
        cache_dir: dir.path().to_path_buf(),
        ephemeral: false,
        ..pruned_config()
    };
    let checkpoint_lowest_retained = Height(3);
    let max_checkpoint_height = Height(MIN_PRUNING_RETENTION + checkpoint_lowest_retained.0 - 1);

    let state = new_state_with_checkpoint_retention(&config, &network, max_checkpoint_height);
    std::mem::drop(state);

    let error = rollback_finalized_state(
        config,
        &network,
        RollbackFinalizedStateOptions {
            target_height: Height(0),
            keep_rolled_back_blocks: false,
            max_checkpoint_height: None,
        },
    )
    .expect_err("rollback cannot remove blocks whose raw transactions were skipped");

    assert!(
        matches!(error, RollbackFinalizedStateError::MissingBlock { height } if height < checkpoint_lowest_retained),
        "rollback reports that skipped raw block data is unavailable: {error:?}"
    );
}

#[test]
fn initial_online_prune_preserves_pre_boundary_history() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;

    let state = new_state_with_blocks(&Config::ephemeral(), &network);
    let (prune_from, prune_until) =
        prune_height_range_inner(TEST_BLOCKS, 5, None).expect("initial prune range exists");

    assert_eq!(
        (prune_from, prune_until),
        (TEST_BLOCKS - 5, TEST_BLOCKS - 5 + 1),
        "initial online prune should only prune from the retention boundary"
    );

    let preserved_tx_hash = coinbase_tx_hash(&network, prune_from - 1);
    let pruned_tx_hash = coinbase_tx_hash(&network, prune_from);

    let mut batch = DiskWriteBatch::new();
    batch.prepare_prune_batch(&state.db, Height(prune_from), Height(prune_until));
    state.db.write_batch(batch).expect("prune batch writes");

    assert!(
        state.db.transaction(preserved_tx_hash).is_some(),
        "raw transaction before the online pruning boundary is preserved"
    );
    assert!(
        state.db.block(Height(prune_from - 1).into()).is_some(),
        "preserved block below the pruning marker is still reconstructed"
    );
    assert!(
        state
            .db
            .block_and_size(Height(prune_from - 1).into())
            .is_some(),
        "preserved block and size below the pruning marker is still available"
    );
    assert!(
        state.db.transaction(pruned_tx_hash).is_none(),
        "raw transaction at the online pruning boundary is pruned"
    );
    assert!(
        state.db.block(Height(prune_from).into()).is_none(),
        "pruned block at the online pruning boundary is not reconstructed"
    );
    assert!(
        state.db.block_and_size(Height(prune_from).into()).is_none(),
        "pruned block and size at the online pruning boundary is unavailable"
    );
    assert_eq!(
        state.db.lowest_retained_height(),
        Some(Height(prune_until)),
        "pruning progress marker advances to the exclusive prune bound"
    );
}

#[test]
fn prepare_prune_batch_deletes_history_and_keeps_consensus_state() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;

    let state = new_state_with_blocks(&Config::ephemeral(), &network);

    // Capture consensus-critical aggregates before pruning.
    let value_pool_before = state.db.finalized_value_pool();

    // The coinbase output of a to-be-pruned block creates a UTXO that must survive,
    // because the UTXO set is consensus-critical and is never pruned.
    let pruned_tx_hash = coinbase_tx_hash(&network, 1);
    let pruned_outpoint = transparent::OutPoint::from_usize(pruned_tx_hash, 0);
    assert!(
        state.db.transaction(pruned_tx_hash).is_some(),
        "raw transaction present before pruning"
    );
    let utxo_before = state.db.utxo(&pruned_outpoint);
    assert!(
        utxo_before.is_some(),
        "coinbase UTXO present before pruning"
    );

    // Prune heights 1..4 (1, 2, 3).
    let mut batch = DiskWriteBatch::new();
    batch.prepare_prune_batch(&state.db, Height(1), Height(4));
    state.db.write_batch(batch).expect("prune batch writes");

    // Raw transaction data is gone for the pruned heights.
    for height in 1..4 {
        let tx_hash = coinbase_tx_hash(&network, height);
        assert!(
            state.db.transaction(tx_hash).is_none(),
            "raw transaction pruned at height {height}"
        );
        assert_eq!(
            state.db.transactions_by_height(Height(height)).count(),
            0,
            "tx_by_loc pruned at height {height}"
        );

        // The transaction location index is intentionally retained: it is needed
        // to resolve spends of UTXOs created in pruned blocks.
        assert!(
            state.db.transaction_location(tx_hash).is_some(),
            "tx_loc_by_hash retained at height {height}"
        );

        // Block headers are NOT pruned.
        assert!(
            state.db.block_header(Height(height).into()).is_some(),
            "block header retained at height {height}"
        );
        assert!(
            state.db.block(Height(height).into()).is_none(),
            "block reconstruction returns None when raw transaction data is pruned at height {height}"
        );
        assert!(
            state.db.block_and_size(Height(height).into()).is_none(),
            "block and size lookup returns None when raw transaction data is pruned at height {height}"
        );
    }

    // Genesis and heights at/above the retention floor still have their tx data.
    assert!(
        state
            .db
            .transaction(coinbase_tx_hash(&network, 0))
            .is_some(),
        "genesis transaction retained"
    );
    for height in 4..=TEST_BLOCKS {
        let block = state
            .db
            .block(Height(height).into())
            .expect("retained block is available");
        assert!(
            !block.transactions.is_empty(),
            "retained block has transactions at height {height}"
        );
        assert!(
            state.db.block_and_size(Height(height).into()).is_some(),
            "retained block and size is available at height {height}"
        );
        assert!(
            state
                .db
                .transaction(coinbase_tx_hash(&network, height))
                .is_some(),
            "transaction retained at height {height}"
        );
    }

    // Consensus-critical state is untouched by pruning.
    assert_eq!(
        state.db.finalized_value_pool(),
        value_pool_before,
        "value pool unchanged by pruning"
    );
    assert_eq!(
        state.db.utxo(&pruned_outpoint).is_some(),
        utxo_before.is_some(),
        "coinbase UTXO retained after its block's tx data was pruned"
    );

    // The pruning marker records progress and marks the database as pruned.
    assert!(state.db.is_pruned(), "database is marked as pruned");
    assert_eq!(
        state.db.lowest_retained_height(),
        Some(Height(4)),
        "lowest retained height advanced to the exclusive prune bound"
    );
}

#[test]
#[should_panic(expected = "pruned")]
fn reopening_pruned_database_in_archive_mode_panics() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;

    let dir = tempfile::tempdir().expect("temp dir is created");
    let config = Config {
        cache_dir: dir.path().to_path_buf(),
        ephemeral: false,
        ..Config::default()
    };

    // Sync and prune, then drop the handle to release the database lock.
    {
        let state = new_state_with_blocks(&config, &network);
        let mut batch = DiskWriteBatch::new();
        batch.prepare_prune_batch(&state.db, Height(1), Height(2));
        state.db.write_batch(batch).expect("prune batch writes");
    }

    // Reopening in archive mode (the default) must refuse, because pruned data
    // can't be served.
    let _state = FinalizedState::new(
        &config,
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
}

#[test]
fn validate_storage_mode_enforces_retention_floor() {
    let pruned = |tx_retention| Config {
        storage_mode: StorageMode::Pruned(PruningConfig { tx_retention }),
        ..Config::default()
    };

    // Archive mode is always valid.
    assert!(Config::default().validate_storage_mode(&Mainnet).is_ok());

    // On Mainnet/Testnet the floor is MIN_PRUNING_RETENTION.
    assert!(pruned(MIN_PRUNING_RETENTION - 1)
        .validate_storage_mode(&Mainnet)
        .is_err());
    assert!(pruned(MIN_PRUNING_RETENTION)
        .validate_storage_mode(&Mainnet)
        .is_ok());

    // Regtest relaxes the floor to MAX_BLOCK_REORG_HEIGHT + 1, so it accepts
    // retentions far below the Mainnet floor, but still rejects anything that
    // does not cover the reorg window.
    let regtest = Network::new_regtest(Default::default());
    let regtest_floor = MAX_BLOCK_REORG_HEIGHT + 1;
    assert!(pruned(regtest_floor - 1)
        .validate_storage_mode(&regtest)
        .is_err());
    assert!(pruned(regtest_floor)
        .validate_storage_mode(&regtest)
        .is_ok());
    assert!(
        pruned(MIN_PRUNING_RETENTION - 1)
            .validate_storage_mode(&regtest)
            .is_ok(),
        "Regtest accepts a retention below the Mainnet floor"
    );
}
