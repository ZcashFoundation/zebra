//! Tests for pruned storage mode.
//!
//! These tests cover:
//! - the pure prune-range arithmetic ([`prune_height_range_inner`]),
//! - the deletion mechanism ([`DiskWriteBatch::prepare_prune_batch`]): historical
//!   transaction data is removed while consensus-critical state survives,
//! - the one-way marker: a pruned database cannot be reopened in archive mode,
//! - configuration validation of the retention floor.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network::{self, Mainnet},
    serialization::ZcashDeserializeInto,
    transparent,
};

use crate::{
    config::StorageMode,
    constants::{MAX_BLOCK_REORG_HEIGHT, MAX_PRUNE_HEIGHTS_PER_COMMIT, MIN_PRUNING_RETENTION},
    service::finalized_state::{disk_db::DiskWriteBatch, FinalizedState},
    Config, PruningConfig,
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
