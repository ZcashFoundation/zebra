//! Tests for syncer status.

use std::{env, sync::Arc, time::Duration};

use futures::{select, FutureExt};
use proptest::prelude::*;
use tokio::{
    sync::{watch, Semaphore},
    time::timeout,
};
use zebra_chain::{
    block::Block, chain_sync_status::ChainSyncStatus, parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_network::PeerSetStatus;
use zebra_state::{ChainTipBlock, ChainTipSender, CheckpointVerifiedBlock};
use zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES;

use super::{super::RecentSyncLengths, SyncStatus};

/// The default number of test cases to run.
const DEFAULT_ASYNC_SYNCHRONIZED_TASKS_PROPTEST_CASES: u32 = 32;

/// The maximum time one test instance should run.
///
/// If the test exceeds this time it is considered to have failed.
const MAX_TEST_EXECUTION: Duration = Duration::from_secs(10);

/// The maximum time to wait for an event to be received.
///
/// If an event is not received in this time, it is considered that it will never be received.
const EVENT_TIMEOUT: Duration = Duration::from_millis(5);

fn build_sync_status(
    min_ready_peers: usize,
    progress_grace: Duration,
) -> (
    SyncStatus,
    RecentSyncLengths,
    watch::Sender<PeerSetStatus>,
    ChainTipSender,
) {
    let (peer_status_tx, peer_status_rx) = watch::channel(PeerSetStatus::default());
    let (tip_sender, latest_chain_tip, _change) =
        ChainTipSender::new(None::<ChainTipBlock>, &Network::Mainnet);
    let (status, recent_sync_lengths) = SyncStatus::new(
        peer_status_rx,
        latest_chain_tip,
        min_ready_peers,
        progress_grace,
    );

    (status, recent_sync_lengths, peer_status_tx, tip_sender)
}

fn genesis_tip() -> ChainTipBlock {
    BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .map(CheckpointVerifiedBlock::from)
        .map(ChainTipBlock::from)
        .expect("valid genesis block")
}

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_ASYNC_SYNCHRONIZED_TASKS_PROPTEST_CASES))
    )]

    /// Test if the [`SyncStatus`] correctly waits until the chain tip is reached.
    ///
    /// This is an asynchronous test with two concurrent tasks. The main task mocks chain sync
    /// length updates and verifies if the other task was awakened by the update.
    #[test]
    fn waits_until_close_to_tip(sync_lengths in any::<Vec<usize>>()) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();

        runtime.block_on(timeout(MAX_TEST_EXECUTION, root_task(sync_lengths)))??;

        /// The root task that the runtime executes.
        ///
        /// Spawns the two concurrent tasks, and sets up the synchronization channels between them.
        async fn root_task(sync_lengths: Vec<usize>) -> Result<(), TestCaseError> {
            let update_events = Arc::new(Semaphore::new(0));
            let wake_events = Arc::new(Semaphore::new(0));

            let (status, recent_sync_lengths, peer_status_tx, _tip_sender) =
                build_sync_status(1, Duration::from_secs(5 * 60));
            let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

            let mut wait_task_handle = tokio::spawn(wait_task(
                status.clone(),
                update_events.clone(),
                wake_events.clone(),
            ))
            .fuse();

            let mut main_task_handle = tokio::spawn(main_task(
                sync_lengths,
                status,
                recent_sync_lengths,
                update_events,
                wake_events,
            ))
            .fuse();

            select! {
                result = main_task_handle => result.expect("Failed to wait for main test task"),
                result = wait_task_handle => result.expect("Failed to wait for wait test task"),
            }
        }

        /// The main task.
        ///
        /// 1. Applies each chain sync length update from the `sync_lengths` parameter.
        /// 2. If necessary, notify the other task that an update was applied. This is to avoid
        ///    having the other task enter an infinite loop while it thinks it has reached the
        ///    chain tip.
        /// 3. Waits to see if the other task sends a wake event, meaning that it awoke because it
        ///    was notified that it has reached the chain tip.
        /// 4. Compares to see if the there was an awake event and if it was expected or not based
        ///    on whether the [`SyncStatus`] says that it's close to the tip.
        async fn main_task(
            sync_lengths: Vec<usize>,
            status: SyncStatus,
            mut recent_sync_lengths: RecentSyncLengths,
            update_events: Arc<Semaphore>,
            wake_events: Arc<Semaphore>,
        ) -> Result<(), TestCaseError> {
            let mut needs_update_event = true;

            for length in sync_lengths {
                recent_sync_lengths.push_extend_tips_length(length);

                if needs_update_event {
                    update_events.add_permits(1);
                }

                let awoke = match timeout(EVENT_TIMEOUT, wake_events.acquire()).await {
                    Ok(permit) => {
                        permit.expect("Semaphore closed prematurely").forget();
                        true
                    }
                    Err(_) => false,
                };

                needs_update_event = awoke;

                assert_eq!(status.is_close_to_tip(), awoke);
            }

            Ok(())
        }

        /// The helper task that repeatedly waits until the chain tip is close.
        ///
        /// 1. Waits for an update event granting permission to run an iteration. This avoids
        ///    looping repeatedly while [`SyncStatus`] reports that it is close to the chain tip.
        /// 2. Waits until [`SyncStatus`] reports that it is close to the chain tip.
        /// 3. Notifies the main task that it awoke, i.e., that the [`SyncStatus`] has finished
        ///    waiting until it was close to the chain tip.
        async fn wait_task(
            mut status: SyncStatus,
            update_events: Arc<Semaphore>,
            wake_events: Arc<Semaphore>,
        ) -> Result<(), TestCaseError> {
            loop {
                update_events.acquire().await.expect("Semaphore closed prematurely").forget();

                // The refactor suggested by clippy is harder to read and understand.
                #[allow(clippy::question_mark)]
                if status.wait_until_close_to_tip().await.is_err() {
                    return Ok(());
                }

                wake_events.add_permits(1);
            }
        }
    }
}

/// Test if totally empty sync lengths array is not near tip.
#[test]
fn empty_sync_lengths() {
    let (status, _recent_sync_lengths, peer_status_tx, _tip_sender) =
        build_sync_status(1, Duration::from_secs(5 * 60));
    let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

    assert!(!status.is_close_to_tip());
}

/// Test if sync lengths array with all zeroes indicates network disconnect, not near tip.
/// This prevents false positives when Zebra loses all peers (issue #4649).
#[test]
fn zero_sync_lengths_means_disconnected() {
    let (status, mut recent_sync_lengths, peer_status_tx, _tip_sender) =
        build_sync_status(1, Duration::from_secs(5 * 60));
    let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

    for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
        recent_sync_lengths.push_extend_tips_length(0);
    }

    assert!(!status.is_close_to_tip());
}

/// Test if sync lengths array with high values is not near tip.
#[test]
fn high_sync_lengths() {
    let (status, mut recent_sync_lengths, peer_status_tx, _tip_sender) =
        build_sync_status(1, Duration::from_secs(5 * 60));
    let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

    // The value 500 is based on the fact that sync lengths are around 500
    // blocks long when Zebra is syncing.
    for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
        recent_sync_lengths.push_extend_tips_length(500);
    }

    assert!(!status.is_close_to_tip());
}

/// Test if sync lengths with mixed small values (including zeros) is near tip.
/// This represents normal operation at the chain tip with occasional new blocks.
#[test]
fn mixed_small_sync_lengths_near_tip() {
    let (status, mut recent_sync_lengths, peer_status_tx, _tip_sender) =
        build_sync_status(1, Duration::from_secs(5 * 60));
    let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

    // Simulate being at tip with occasional new blocks
    recent_sync_lengths.push_extend_tips_length(0);
    recent_sync_lengths.push_extend_tips_length(2);
    recent_sync_lengths.push_extend_tips_length(1);

    // Average is (0 + 2 + 1) / 3 = 1, which is < MIN_DIST_FROM_TIP (20)
    assert!(status.is_close_to_tip());
}

/// Tip changes should count as recent activity even when sync batches are zero.
#[test]
fn tip_updates_count_as_activity() {
    let (status, mut recent_sync_lengths, peer_status_tx, mut tip_sender) =
        build_sync_status(1, Duration::from_secs(5 * 60));
    let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

    // Three zero batches without activity should look like "still syncing".
    for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
        recent_sync_lengths.push_extend_tips_length(0);
    }
    assert!(!status.is_close_to_tip());

    // Advancing the finalized tip should register as progress even though batches are zero.
    tip_sender.set_finalized_tip(genesis_tip());
    assert!(status.is_close_to_tip());
}

/// If no activity happens within the grace window, readiness should eventually flip to false.
#[test]
fn activity_expires_after_grace() {
    let grace = Duration::from_millis(200);
    let (status, mut recent_sync_lengths, peer_status_tx, mut tip_sender) =
        build_sync_status(1, grace);
    let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

    // Tip update marks activity.
    tip_sender.set_finalized_tip(genesis_tip());
    recent_sync_lengths.push_extend_tips_length(0);
    assert!(status.is_close_to_tip());

    // After the grace period elapses, zero batches should make us report syncing again.
    std::thread::sleep(std::time::Duration::from_millis(400));
    recent_sync_lengths.push_extend_tips_length(0);
    assert!(!status.is_close_to_tip());
}

/// Test if sync lengths with a mix including some progress is near tip.
#[test]
fn small_nonzero_sync_lengths_near_tip() {
    let (status, mut recent_sync_lengths, peer_status_tx, _tip_sender) =
        build_sync_status(1, Duration::from_secs(5 * 60));
    let _ = peer_status_tx.send(PeerSetStatus::new(1, 1));

    // Small sync batches when near tip
    recent_sync_lengths.push_extend_tips_length(5);
    recent_sync_lengths.push_extend_tips_length(3);
    recent_sync_lengths.push_extend_tips_length(4);

    // Average is 4, which is < MIN_DIST_FROM_TIP (20)
    assert!(status.is_close_to_tip());
}

/// Test that lack of ready peers blocks the near-tip signal.
#[test]
fn zero_ready_peers_prevent_tip_flag() {
    let (status, mut recent_sync_lengths, _peer_status_tx, _tip_sender) =
        build_sync_status(1, Duration::from_secs(5 * 60));

    recent_sync_lengths.push_extend_tips_length(2);
    recent_sync_lengths.push_extend_tips_length(1);
    recent_sync_lengths.push_extend_tips_length(3);

    assert!(!status.is_close_to_tip());
}

/// Ensure min_ready_peers=0 (regtest) still reports near tip.
#[test]
fn regtest_allows_zero_peers() {
    let (status, mut recent_sync_lengths, _peer_status_tx, _tip_sender) =
        build_sync_status(0, Duration::from_secs(5 * 60));

    recent_sync_lengths.push_extend_tips_length(1);
    recent_sync_lengths.push_extend_tips_length(1);
    recent_sync_lengths.push_extend_tips_length(1);

    assert!(status.is_close_to_tip());
}
