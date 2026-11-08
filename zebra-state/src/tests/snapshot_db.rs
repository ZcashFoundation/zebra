//! A snapshot-shaped finalized database for tests.
//!
//! The snapshot importer is not needed here: a Regtest chain is committed block by block, then
//! every block row below the snapshot window and every body below the snapshot height is
//! deleted, and the `snapshot_base` marker is written. That is the shape DESIGN §3 and §7
//! describe, with the bodies that snapshot formats 2 and 3 ship: genesis, the hashes and headers of the window `L..=H`,
//! the body of `H`, the `block_info` rows from `L - 1`, the validation state at `H`, and the
//! transaction index pairs of every transaction that still has an unspent output.

use std::{path::PathBuf, sync::Arc};

use zebra_chain::{
    amount::Amount,
    block::{self, Block, ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    state_snapshot::ObjectHash,
    transaction::{LockTime, Transaction},
    transparent,
};

use crate::{
    service::finalized_state::{
        DiskWriteBatch, FinalizedState, TransactionLocation, WriteDisk, ZebraDb,
    },
    state_database_format_version_in_code,
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config, SnapshotBase, MAX_BLOCK_REORG_HEIGHT,
};

/// The smallest retention Regtest allows, so pruning tests stay small.
pub(crate) const RETENTION: u32 = MAX_BLOCK_REORG_HEIGHT + 1;

/// The manifest hash every test marker carries.
pub(crate) const MANIFEST: ObjectHash = ObjectHash([0x5a; 32]);

/// The value of every test coinbase output, in zatoshis.
pub(crate) const COINBASE_VALUE: i64 = 1_000;

/// The coinbase transaction kind of a test block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Coinbase {
    /// A V5 coinbase, which carries a consensus branch ID.
    Branched,
    /// A V4 coinbase, which carries no branch ID: a window without any branch ID transaction.
    Legacy,
}

/// A Regtest network with NU5 active from height 1 and no funding streams.
pub(crate) fn network() -> Network {
    Network::new_regtest(zebra_chain::parameters::testnet::RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            ..Default::default()
        },
        funding_streams: Some(Vec::new()),
        ..Default::default()
    })
}

/// A state config at `cache_dir`, pruning when `retention` is set.
pub(crate) fn config(cache_dir: PathBuf, retention: Option<u32>) -> Config {
    Config {
        cache_dir,
        prune_blocks_older_than: retention,
        should_backup_non_finalized_state: false,
        ..Config::default()
    }
}

/// Opens the state with format checks skipped and marked finished, as an import leaves it.
pub(crate) fn open(config: &Config) -> Result<FinalizedState, crate::StateInitError> {
    let state = FinalizedState::new_with_debug(
        config,
        &network(),
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    state
        .db
        .update_format_version_on_disk(&state_database_format_version_in_code())
        .expect("the test cache directory is writable");
    state.db.mark_finished_format_upgrades();
    Ok(state)
}

/// Returns a coinbase transaction of `kind` for `height`, paying [`COINBASE_VALUE`] to a
/// pay-to-public-key-hash address so later blocks can spend it.
fn coinbase(height: Height, kind: Coinbase) -> Arc<Transaction> {
    let input = transparent::Input::Coinbase {
        height,
        data: vec![0],
        sequence: u32::MAX,
    };
    let output = transparent::Output {
        value: Amount::try_from(COINBASE_VALUE).expect("a small positive amount"),
        lock_script: transparent::Script::new(&[
            0x76, 0xa9, 0x14, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a,
            0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x88, 0xac,
        ]),
    };
    match kind {
        Coinbase::Branched => Arc::new(Transaction::test_v5(
            NetworkUpgrade::current(&network(), height),
            vec![input],
            vec![output],
            LockTime::unlocked(),
            height,
        )),
        Coinbase::Legacy => Arc::new(Transaction::test_v4(
            vec![input],
            vec![output],
            LockTime::unlocked(),
            height,
        )),
    }
}

/// Returns a transaction spending the coinbase output of the block at `height`, with no outputs.
pub(crate) fn spend_coinbase_of(db: &ZebraDb, height: Height, expiry: Height) -> Arc<Transaction> {
    let coinbase = db
        .block(height.into())
        .expect("the spent block has a body")
        .expect("the spent block is in the chain")
        .transactions[0]
        .clone();
    Arc::new(Transaction::test_v5(
        NetworkUpgrade::current(&network(), expiry),
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: coinbase.hash(),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[]),
            sequence: 0,
        }],
        vec![],
        LockTime::unlocked(),
        expiry,
    ))
}

/// Returns a child of `parent` whose commitment covers the history tree root `root`, with a
/// coinbase of `kind` followed by `extra` transactions.
pub(crate) fn child(
    parent: &Arc<Block>,
    root: ChainHistoryMmrRootHash,
    kind: Coinbase,
    extra: Vec<Arc<Transaction>>,
) -> Arc<Block> {
    let height = parent
        .coinbase_height()
        .expect("test blocks have coinbase heights")
        .next()
        .expect("test chains stay below the maximum height");
    let mut transactions = vec![coinbase(height, kind)];
    transactions.extend(extra);
    let mut block = Block {
        header: parent.header.clone(),
        transactions,
    };
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = parent.hash();
    header.time += chrono::Duration::seconds(75);
    header.merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
    let commitment =
        ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&root, &block.auth_data_root());
    Arc::new(block).set_block_commitment(commitment.into())
}

/// Returns the history tree root a child of the finalized tip commits to.
pub(crate) fn tip_history_root(db: &ZebraDb) -> ChainHistoryMmrRootHash {
    db.history_tree()
        .hash()
        .unwrap_or_else(|| block::CHAIN_HISTORY_ACTIVATION_RESERVED.into())
}

/// Commits one child of the finalized tip, with a coinbase of `kind` and `extra` transactions.
pub(crate) fn commit_child(
    state: &mut FinalizedState,
    kind: Coinbase,
    extra: Vec<Arc<Transaction>>,
) -> Arc<Block> {
    let parent = state
        .db
        .tip_block()
        .expect("the tip body is complete")
        .expect("the chain has a tip");
    let block = child(&parent, tip_history_root(&state.db), kind, extra);
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "snapshot database test",
        )
        .expect("test blocks commit");
    block
}

/// Extends the chain to `tip` with coinbase-only blocks of `kind`, from genesis if needed.
pub(crate) fn append(state: &mut FinalizedState, tip: u32, kind: Coinbase) {
    if state.db.is_empty() {
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block::genesis::regtest_genesis_block()).into(),
                None,
                "snapshot database test",
            )
            .expect("the Regtest genesis block commits");
    }
    while state
        .db
        .finalized_tip_height()
        .expect("genesis is committed")
        .0
        < tip
    {
        commit_child(state, kind, vec![]);
    }
}

/// Deletes every block row below `window_start` except genesis, keeping the `block_info` row
/// just below the window and every transaction index pair, and every body below the finalized
/// tip except genesis, then writes the marker for a snapshot at the finalized tip whose
/// `deferred_checks` are still pending.
pub(crate) fn carve(
    state: &FinalizedState,
    window_start: Height,
    deferred_checks: u8,
) -> SnapshotBase {
    let db = &state.db;
    let (height, block_hash) = db.tip().expect("the chain has a tip");
    assert!(
        Height(1) <= window_start && window_start <= height,
        "the window must start above genesis and at or below the tip"
    );
    let hash_by_height = db
        .cf_handle("hash_by_height")
        .expect("column family exists");
    let height_by_hash = db
        .cf_handle("height_by_hash")
        .expect("column family exists");
    let headers = db
        .cf_handle("block_header_by_height")
        .expect("column family exists");
    let block_info = db.cf_handle("block_info").expect("column family exists");
    let mut batch = DiskWriteBatch::new();
    for below in 1..window_start.0 {
        let below = Height(below);
        let hash = db
            .hash(below)
            .expect("the chain is complete before carving");
        batch.zs_delete(&hash_by_height, below);
        batch.zs_delete(&height_by_hash, hash);
        batch.zs_delete(&headers, below);
        if below.next().expect("below the tip") < window_start {
            batch.zs_delete(&block_info, below);
        }
    }
    batch.zs_delete_range(
        &db.cf_handle("tx_by_loc").expect("column family exists"),
        TransactionLocation::min_for_height(Height(1)),
        TransactionLocation::min_for_height(height),
    );
    let base = SnapshotBase {
        window_start,
        height,
        block_hash,
        manifest: MANIFEST,
        deferred_checks,
    };
    write_marker(&mut batch, db, &base);
    db.write_batch(batch).expect("the batch writes");
    base
}

/// Stages `base` as the database's snapshot marker.
pub(crate) fn write_marker(batch: &mut DiskWriteBatch, db: &ZebraDb, base: &SnapshotBase) {
    let _ = db
        .snapshot_base_cf()
        .with_batch_for_writing(batch)
        .zs_insert(&(), base);
}

/// Builds a snapshot-shaped database at `cache_dir`: a chain of `tip` coinbase-only blocks of
/// `kind`, carved to the window starting at `window_start`, with `deferred_checks` pending.
pub(crate) fn snapshot_state(
    cache_dir: PathBuf,
    tip: u32,
    window_start: u32,
    kind: Coinbase,
    deferred_checks: u8,
) -> (FinalizedState, SnapshotBase) {
    let mut state = open(&config(cache_dir, Some(RETENTION))).expect("a fresh state opens");
    append(&mut state, tip, kind);
    let base = carve(&state, Height(window_start), deferred_checks);
    (state, base)
}
