//! Retention boundaries, worker ownership, and restart behavior.

use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

use zebra_chain::{
    block::{self, Block, ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    transaction::{LockTime, Transaction},
    transparent::Input,
};

use super::*;
use crate::{
    arbitrary::Prepare,
    service::{
        finalized_state::FinalizedState, non_finalized_state::NonFinalizedState, StateService,
    },
    state_database_format_version_in_code,
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config, MAX_BLOCK_REORG_HEIGHT,
};

const RETENTION: u32 = MAX_BLOCK_REORG_HEIGHT + 1;
const TIP: u32 = RETENTION + 249;

fn network() -> Network {
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

fn config(cache_dir: PathBuf) -> Config {
    Config {
        cache_dir,
        prune_blocks_older_than: Some(RETENTION),
        should_backup_non_finalized_state: false,
        ..Config::default()
    }
}

fn open(config: &Config) -> FinalizedState {
    let state = FinalizedState::new_with_debug(
        config,
        &network(),
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    state
        .db
        .update_format_version_on_disk(&state_database_format_version_in_code())
        .unwrap();
    state.db.mark_finished_format_upgrades();
    state
}

fn child(parent: &Arc<Block>, root: ChainHistoryMmrRootHash) -> Arc<Block> {
    let height = parent.coinbase_height().unwrap().next().unwrap();
    let mut block = Block {
        header: parent.header.clone(),
        transactions: vec![Arc::new(Transaction::test_v5(
            NetworkUpgrade::current(&network(), height),
            vec![Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![],
            LockTime::unlocked(),
            height,
        ))],
    };
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = parent.hash();
    header.time += chrono::Duration::seconds(75);
    header.merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
    let commitment =
        ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&root, &block.auth_data_root());
    Arc::new(block).set_block_commitment(commitment.into())
}

fn append(state: &mut FinalizedState, tip: u32) {
    if state.db.is_empty() {
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block::genesis::regtest_genesis_block()).into(),
                None,
                "pruning worker test",
            )
            .unwrap();
    }
    while state.db.finalized_tip_height().unwrap().0 < tip {
        let parent = state.db.tip_block().unwrap().unwrap();
        let root = state
            .db
            .history_tree()
            .hash()
            .unwrap_or_else(|| block::CHAIN_HISTORY_ACTIVATION_RESERVED.into());
        state
            .commit_finalized_direct(child(&parent, root).into(), None, "pruning worker test")
            .unwrap();
    }
}

fn populated(config: &Config) -> FinalizedState {
    let mut state = open(config);
    append(&mut state, TIP);
    state
}

fn floor(db: &ZebraDb) -> Height {
    db.history_capabilities().unwrap().lowest_retained_height
}

fn wait_for_floor(db: &ZebraDb, expected: Height, task: &PruningTask) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while floor(db) != expected {
        task.check_for_panics();
        assert!(
            Instant::now() < deadline,
            "pruning did not reach {expected:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn retention_uses_the_durable_tip_and_excludes_genesis() {
    for (tip, retained, expected) in [
        (0, RETENTION, 1),
        (RETENTION - 1, RETENTION, 1),
        (RETENTION, RETENTION, 1),
        (RETENTION + 1, RETENTION, 2),
        (TIP, RETENTION, 250),
        (Height::MAX.0, u32::MAX, 1),
        (Height::MAX.0, 10_000, Height::MAX.0 + 1 - 10_000),
    ] {
        assert_eq!(retention_floor(Height(tip), retained), Height(expected));
    }
}

#[test]
fn archive_conversion_is_bounded_and_retention_never_restores_bodies() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let state = populated(&config(cache.path().to_path_buf()));
    state.db.mark_ready_for_pruning();
    assert!(prune_batch(&state.db, RETENTION, &mut Default::default()).unwrap());
    assert_eq!(floor(&state.db), Height(101));
    assert!(state.db.block(Height(0).into()).unwrap().is_some());
    assert!(state.db.block(Height(1).into()).is_err());
    assert!(state.db.block(Height(101).into()).unwrap().is_some());

    assert!(!prune_batch(&state.db, TIP + 1, &mut Default::default()).unwrap());
    assert_eq!(floor(&state.db), Height(101));
    while prune_batch(&state.db, RETENTION, &mut Default::default()).unwrap() {}
    assert_eq!(floor(&state.db), Height(250));
    assert!(state.db.block(Height(TIP).into()).unwrap().is_some());
}

#[test]
fn committed_batches_resume_after_process_exit() {
    const CHILD_CACHE: &str = "ZEBRA_PRUNING_CRASH_TEST_CACHE";
    let _init_guard = zebra_test::init();
    if let Some(cache) = std::env::var_os(CHILD_CACHE) {
        let state = populated(&config(cache.into()));
        state.db.mark_ready_for_pruning();
        assert!(prune_batch(&state.db, RETENTION, &mut Default::default()).unwrap());
        assert_eq!(floor(&state.db), Height(101));
        // Skip database and worker destructors, as an abrupt process exit would.
        std::process::exit(0);
    }

    let cache = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "service::pruning::tests::committed_batches_resume_after_process_exit",
            "--nocapture",
        ])
        .env(CHILD_CACHE, cache.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("pruning crash fixture did not finish");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    let state = open(&config(cache.path().to_path_buf()));
    assert_eq!(floor(&state.db), Height(101));
    state.db.mark_ready_for_pruning();
    assert!(prune_batch(&state.db, RETENTION, &mut Default::default()).unwrap());
    assert_eq!(floor(&state.db), Height(201));
    while prune_batch(&state.db, RETENTION, &mut Default::default()).unwrap() {}
    assert_eq!(floor(&state.db), Height(250));
}

#[test]
fn worker_waits_for_readiness_and_wakes_for_finalization() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let mut state = populated(&config(cache.path().to_path_buf()));
    let (wake, receiver) = bounded(1);
    let task = PruningTask::spawn(state.db.clone(), RETENTION, receiver);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(floor(&state.db), Height(0));
    state.db.mark_ready_for_pruning();
    wait_for_floor(&state.db, Height(250), &task);
    append(&mut state, TIP + 1);
    wake.try_send(()).unwrap();
    wait_for_floor(&state.db, Height(251), &task);
    task.stop();
    assert!(task.task.lock().unwrap().is_none());
    assert!(matches!(
        wake.try_send(()),
        Err(crossbeam_channel::TrySendError::Disconnected(()))
    ));
}

#[test]
fn shutdown_joins_a_worker_waiting_for_readiness() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let state = populated(&config(cache.path().to_path_buf()));
    let (wake, receiver) = bounded(1);
    let task = PruningTask::spawn(state.db.clone(), RETENTION, receiver);
    task.stop();
    state.db.mark_ready_for_pruning();
    assert!(matches!(
        wake.try_send(()),
        Err(crossbeam_channel::TrySendError::Disconnected(()))
    ));
    assert_eq!(floor(&state.db), Height(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_prunes_existing_history_at_startup_and_joins_on_shutdown() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let cache = tempfile::tempdir().unwrap();
        let config = config(cache.path().to_path_buf());
        let populate_config = config.clone();
        tokio::task::spawn_blocking(move || drop(populated(&populate_config)))
            .await
            .unwrap();
        let (state, reader, _, _) = StateService::new(config, &network(), Height(TIP), 1).await;
        let task = reader.pruning_task.clone().unwrap();
        let db = reader.db.clone();
        let observer = task.clone();
        tokio::task::spawn_blocking(move || wait_for_floor(&db, Height(250), &observer))
            .await
            .unwrap();
        tokio::task::spawn_blocking(move || drop(state))
            .await
            .unwrap();
        assert!(task.task.lock().unwrap().is_none());
        assert!(reader.db.block(Height(TIP).into()).unwrap().is_some());
    })
    .await
    .unwrap();
}

#[test]
fn pruning_preserves_non_finalized_forks_and_reorg_validation() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let finalized = populated(&config(cache.path().to_path_buf()));
    let parent = finalized.db.tip_block().unwrap().unwrap();
    let root = finalized.db.history_tree().hash().unwrap();
    let first = child(&parent, root);
    let competing = first.clone().set_work(100);
    let mut archive = NonFinalizedState::new(&network());
    archive
        .commit_new_chain(first.prepare(), &finalized.db)
        .unwrap();
    let mut pruned = archive.clone();
    archive
        .commit_new_chain(competing.clone().prepare(), &finalized.db)
        .unwrap();
    finalized.db.mark_ready_for_pruning();
    while prune_batch(&finalized.db, RETENTION, &mut Default::default()).unwrap() {}
    pruned
        .commit_new_chain(competing.clone().prepare(), &finalized.db)
        .unwrap();
    assert!(archive.eq_internal_state(&pruned));
    assert_eq!(pruned.best_tip_block().unwrap().hash, competing.hash());
    archive.invalidate_block(competing.hash()).unwrap();
    pruned.invalidate_block(competing.hash()).unwrap();
    assert!(archive.eq_internal_state(&pruned));
}
