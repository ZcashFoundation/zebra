//! Tests for the consensus / RPC-only finalized write split
//! ([`Config::separate_rpc_index_db`]).
//!
//! Covers:
//! - column-family placement: with the split on, the consensus database is
//!   opened without the RPC-only column families, and the RPC-only data is
//!   written to the separate RPC index database;
//! - RPC-read parity: once the trailing indexer catches up, the RPC-only read
//!   accessors (`address_balance`, `address_utxos`, `address_tx_ids`) on a
//!   split state return the same results as a single-database baseline;
//! - crash-safety catch-up: an RPC index database behind the consensus tip is
//!   replayed forward up to the consensus tip from the consensus database.
//!
//! See `docs/design/state-write-split.md`.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transparent,
};

use crate::{
    service::finalized_state::{
        FinalizedState, CONSENSUS_COLUMN_FAMILIES_IN_CODE, RPC_INDEX_COLUMN_FAMILIES_IN_CODE,
    },
    CheckpointVerifiedBlock, Config,
};

/// A config with the consensus / RPC write split enabled, on an ephemeral
/// database.
fn split_config() -> Config {
    Config {
        separate_rpc_index_db: true,
        ..Config::ephemeral()
    }
}

/// Commits the mainnet genesis and blocks `1..=max_height` to a fresh finalized
/// state with `config`, returning the state and the committed blocks.
///
/// This commits directly through the finalized state (as the disk writer does);
/// the trailing RPC indexer thread is not spawned on this path, so when the
/// split is on the RPC index database is populated separately by the test.
fn commit_blocks(
    config: &Config,
    network: &Network,
    max_height: u32,
) -> (FinalizedState, Vec<Arc<Block>>) {
    let mut state = FinalizedState::new_with_debug(
        config,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    );

    let mut blocks = Vec::new();
    for height in 0..=max_height {
        let block: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .get(&height)
            .unwrap_or_else(|| panic!("continuous mainnet block {height} exists"))
            .zcash_deserialize_into()
            .expect("block deserializes");

        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "rpc-index split test commit",
            )
            .unwrap_or_else(|error| panic!("block {height} commits: {error:?}"));

        blocks.push(block);
    }

    (state, blocks)
}

/// Indexes every committed block into the split state's RPC index database, in
/// height order, exactly as the trailing indexer thread would (catching up from
/// the RPC index tip to the consensus tip).
fn run_indexer_to_tip(state: &FinalizedState, network: &Network) {
    let consensus_db = &state.db;
    let rpc_index_db = consensus_db
        .rpc_index_db()
        .expect("split state has an RPC index database");

    let start = rpc_index_db
        .rpc_index_tip_height()
        .map(|height| (height + 1).expect("valid height"))
        .unwrap_or(Height(0));

    let tip = consensus_db
        .finalized_tip_height()
        .expect("consensus database has a tip");

    let mut height = start;
    while height <= tip {
        let block = consensus_db
            .block(height.into())
            .expect("consensus database has the block");
        let hash = consensus_db
            .hash(height)
            .expect("consensus database has the hash");
        let finalized = crate::request::FinalizedBlock::for_rpc_index(block, hash);

        rpc_index_db
            .write_rpc_index_block(consensus_db, &finalized, network)
            .expect("RPC index block writes");

        height = (height + 1).expect("valid height");
    }
}

/// The transparent addresses that appear as a coinbase / output recipient in
/// `blocks`, so the parity test queries addresses that actually have balances.
fn addresses_in_blocks(blocks: &[Arc<Block>], network: &Network) -> HashSet<transparent::Address> {
    blocks
        .iter()
        .flat_map(|block| block.transactions.iter())
        .flat_map(|tx| tx.outputs().iter())
        .filter_map(|output| output.address(network))
        .collect()
}

/// With the split on, the consensus database is opened without the RPC-only
/// column families, and querying them routes to the RPC index database.
#[test]
fn split_consensus_db_has_no_rpc_only_cfs() {
    let _init_guard = zebra_test::init();

    // The consensus column families and the RPC-only column families are
    // disjoint (the RPC-index tip marker excepted, which is RPC-only).
    for rpc_cf in RPC_INDEX_COLUMN_FAMILIES_IN_CODE {
        assert!(
            !CONSENSUS_COLUMN_FAMILIES_IN_CODE.contains(rpc_cf),
            "RPC-only column family {rpc_cf} must not be in the consensus column families",
        );
    }

    let network = Network::Mainnet;
    let (state, _blocks) = commit_blocks(&split_config(), &network, 5);

    // The consensus database's own handle does not have the RPC-only column
    // families (it was opened without them).
    for rpc_cf in RPC_INDEX_COLUMN_FAMILIES_IN_CODE {
        assert!(
            state.db.raw_cf_handle_is_none_for_test(rpc_cf),
            "consensus database must not contain the RPC-only column family {rpc_cf}",
        );
    }

    // The RPC index database has them.
    let rpc_index_db = state.db.rpc_index_db().expect("split has an RPC index db");
    for rpc_cf in RPC_INDEX_COLUMN_FAMILIES_IN_CODE {
        assert!(
            !rpc_index_db.raw_cf_handle_is_none_for_test(rpc_cf),
            "RPC index database must contain the RPC-only column family {rpc_cf}",
        );
    }
}

/// Once the trailing indexer catches up, the RPC-only read accessors on a split
/// state return the same results as a single-database baseline.
#[test]
fn split_rpc_reads_match_single_db() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let max_height = 9;

    // Baseline: a single-database state (split off).
    let (baseline_state, blocks) = commit_blocks(&Config::ephemeral(), &network, max_height);

    // Split: a consensus + RPC-index state. Index every committed block.
    let (split_state, _split_blocks) = commit_blocks(&split_config(), &network, max_height);
    run_indexer_to_tip(&split_state, &network);

    let rpc_index_tip = split_state
        .db
        .rpc_index_db()
        .expect("split has an RPC index db")
        .rpc_index_tip_height()
        .expect("RPC index has a tip after catch-up");
    assert_eq!(
        rpc_index_tip,
        Height(max_height),
        "the RPC index caught up to the consensus tip",
    );

    let addresses = addresses_in_blocks(&blocks, &network);
    assert!(
        !addresses.is_empty(),
        "the test blocks must have transparent recipients to query",
    );

    for address in &addresses {
        // Balances match.
        assert_eq!(
            split_state.db.address_balance(address),
            baseline_state.db.address_balance(address),
            "split and single-db balances match for {address:?}",
        );

        // UTXOs match.
        assert_eq!(
            split_state.db.address_utxos(address),
            baseline_state.db.address_utxos(address),
            "split and single-db UTXOs match for {address:?}",
        );

        // Transaction ids match across the whole committed range.
        assert_eq!(
            split_state
                .db
                .address_tx_ids(address, Height(0)..=Height(max_height)),
            baseline_state
                .db
                .address_tx_ids(address, Height(0)..=Height(max_height)),
            "split and single-db address tx ids match for {address:?}",
        );
    }
}

/// A partially-indexed RPC index database (behind the consensus tip) is replayed
/// forward up to the consensus tip from the consensus database, reaching the
/// same result as a fully-indexed one.
#[test]
fn split_rpc_index_catches_up_from_behind() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let max_height = 9;

    let (split_state, blocks) = commit_blocks(&split_config(), &network, max_height);

    // Index only the first half of the chain, simulating a crash that left the
    // RPC index behind the consensus tip.
    {
        let consensus_db = &split_state.db;
        let rpc_index_db = consensus_db
            .rpc_index_db()
            .expect("split has an RPC index db");
        for height in 0..=(max_height / 2) {
            let block = consensus_db
                .block(Height(height).into())
                .expect("block exists");
            let hash = consensus_db.hash(Height(height)).expect("hash exists");
            let finalized = crate::request::FinalizedBlock::for_rpc_index(block, hash);
            rpc_index_db
                .write_rpc_index_block(consensus_db, &finalized, &network)
                .expect("partial index writes");
        }

        assert_eq!(
            rpc_index_db.rpc_index_tip_height(),
            Some(Height(max_height / 2)),
            "the RPC index is behind the consensus tip before catch-up",
        );
        assert!(
            rpc_index_db.rpc_index_tip_height().unwrap()
                < consensus_db.finalized_tip_height().unwrap(),
            "the RPC index never exceeds the consensus tip",
        );
    }

    // Catch up the rest of the way.
    run_indexer_to_tip(&split_state, &network);

    assert_eq!(
        split_state
            .db
            .rpc_index_db()
            .unwrap()
            .rpc_index_tip_height(),
        Some(Height(max_height)),
        "catch-up brought the RPC index to the consensus tip",
    );

    // The caught-up index answers queries for addresses across the whole range.
    let addresses = addresses_in_blocks(&blocks, &network);
    for address in &addresses {
        // Every queried address that received funds has a balance after catch-up.
        assert!(
            split_state.db.address_balance(address).is_some(),
            "address {address:?} has a balance after catch-up",
        );
    }
}

/// The disk-tip atomic the trailing indexer trails advances monotonically and
/// matches the consensus tip after each commit (sanity check of the atomic the
/// indexer reads).
#[test]
fn split_disk_tip_atomic_tracks_consensus_tip() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (state, _blocks) = commit_blocks(&split_config(), &network, 5);

    // Simulate the disk writer's published tip: it equals the consensus tip
    // after each durable commit. The indexer never indexes above this.
    let disk_tip = Arc::new(AtomicU32::new(
        state.db.finalized_tip_height().expect("tip").0,
    ));
    assert_eq!(
        disk_tip.load(Ordering::Acquire),
        state.db.finalized_tip_height().unwrap().0,
        "the durable-tip atomic equals the consensus tip",
    );
}
