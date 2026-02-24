//! Tests for syncer status.

use std::{env, sync::Arc, time::Duration};

use futures::{select, FutureExt};
use proptest::prelude::*;
use tokio::{sync::Semaphore, time::timeout};
use zebra_chain::chain_sync_status::ChainSyncStatus;

use zebra_chain::parameters::Network;

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

            let (status, recent_sync_lengths) = SyncStatus::new(&Network::Mainnet);

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
    let (status, _recent_sync_lengths) = SyncStatus::new(&Network::Mainnet);

    assert!(!status.is_close_to_tip());
}

/// Test if sync lengths array with all zeroes is near tip.
#[test]
fn zero_sync_lengths() {
    let (status, mut recent_sync_lengths) = SyncStatus::new(&Network::Mainnet);

    for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
        recent_sync_lengths.push_extend_tips_length(0);
    }

    assert!(status.is_close_to_tip());
}

/// Test if sync lengths array with high values is not near tip.
#[test]
fn high_sync_lengths() {
    let (status, mut recent_sync_lengths) = SyncStatus::new(&Network::Mainnet);

    // The value 500 is based on the fact that sync lengths are around 500
    // blocks long when Zebra is syncing.
    for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
        recent_sync_lengths.push_extend_tips_length(500);
    }

    assert!(!status.is_close_to_tip());
}

/// Test that regtest mode always reports close to tip, even with empty sync lengths.
#[test]
fn regtest_always_close_to_tip_when_empty() {
    let (status, _recent_sync_lengths) = SyncStatus::new(&Network::new_regtest(Default::default()));

    assert!(status.is_close_to_tip());
}

/// Test that regtest mode always reports close to tip, even with high sync lengths.
#[test]
fn regtest_always_close_to_tip_when_syncing() {
    let (status, mut recent_sync_lengths) = SyncStatus::new(&Network::new_regtest(Default::default()));

    for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
        recent_sync_lengths.push_extend_tips_length(500);
    }

    assert!(status.is_close_to_tip());
}

/// Test that `wait_until_close_to_tip()` returns immediately on regtest,
/// without needing any sync length updates.
#[tokio::test]
async fn regtest_wait_until_close_to_tip_returns_immediately() {
    let (mut status, _recent_sync_lengths) = SyncStatus::new(&Network::new_regtest(Default::default()));

    // This should return immediately without blocking, because regtest
    // always considers itself close to the tip.
    tokio::time::timeout(
        Duration::from_millis(100),
        status.wait_until_close_to_tip(),
    )
    .await
    .expect("wait_until_close_to_tip should return immediately on regtest")
    .expect("wait_until_close_to_tip should not return an error");
}
