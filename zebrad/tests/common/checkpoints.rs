//! Test generating checkpoints using `zebra-checkpoints` directly connected to `zebrad`.
//!
//! When running on `Mainnet`, this test requires a cached chain state that is synchronized
//! close to the network chain tip height. It will finish the sync and update the cached chain
//! state.
//!
//! When running on `Testnet`, the cached chain state is optional, but it does speed up the test
//! significantly.

use std::{
    env,
    net::SocketAddr,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use color_eyre::eyre::Result;
use tempfile::TempDir;

use zebra_chain::{
    block::{Height, HeightDiff, TryIntoHeight},
    parameters::Network,
    transparent::MIN_TRANSPARENT_COINBASE_MATURITY,
};
use zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_test::{
    args,
    command::{Arguments, TestDirExt, NO_MATCHES_REGEX_ITER},
    prelude::TestChild,
};

use crate::common::{
    launch::spawn_zebrad_for_rpc, sync::SYNC_FINISHED_REGEX, test_type::TestType::*,
};

use super::{
    config::testdir,
    failure_messages::{
        PROCESS_FAILURE_MESSAGES, ZEBRA_CHECKPOINTS_FAILURE_MESSAGES, ZEBRA_FAILURE_MESSAGES,
    },
    launch::ZebradTestDirExt,
    test_type::TestType,
};

/// The test entry point.
pub async fn run(network: Network) -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a Zebra state dir, but we don't need `lightwalletd`.
    let test_type = UpdateZebraCachedStateWithRpc;
    let test_name = "zebra_checkpoints_test";

    // Skip the test unless the user supplied the correct cached state env vars
    let Some(zebrad_state_path) = test_type.zebrad_state_path(test_name) else {
        return Ok(());
    };

    tracing::info!(
        ?network,
        ?test_type,
        ?zebrad_state_path,
        "running zebra_checkpoints test, spawning zebrad...",
    );

    // Sync zebrad to the network chain tip
    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, true)?
    {
        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    let zebra_rpc_address = zebra_rpc_address.expect("zebra_checkpoints test must have RPC port");

    tracing::info!(
        ?test_type,
        ?zebra_rpc_address,
        "spawned zebrad, waiting for zebrad to open its RPC port..."
    );
    zebrad.expect_stdout_line_matches(&format!("Opened RPC endpoint at {zebra_rpc_address}"))?;

    tracing::info!(
        ?zebra_rpc_address,
        "zebrad opened its RPC port, waiting for it to sync...",
    );
    zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

    let zebra_tip_height = zebrad_tip_height(zebra_rpc_address).await?;

    tracing::info!(
        ?zebra_rpc_address,
        ?zebra_tip_height,
        "zebrad synced to the tip, launching zebra-checkpoints...",
    );
    let zebra_checkpoints = spawn_zebra_checkpoints_direct(network, test_type, zebra_rpc_address)?;

    const LOG_ZEBRAD_CHECKPOINTS: &str = "LOG_ZEBRAD_CHECKPOINTS";

    tracing::info!(
        ?zebra_rpc_address,
        ?zebra_tip_height,
        "spawned zebra-checkpoints connected to zebrad, checkpoints should appear here..."
    );
    tracing::info!(
        "zebrad logs are hidden, show them using {LOG_ZEBRAD_CHECKPOINTS}=1 and RUST_LOG=debug\n\n"
    );

    let (_zebra_checkpoints, _zebrad) = wait_for_zebra_checkpoints_generation(
        zebra_checkpoints,
        zebrad,
        zebra_tip_height,
        test_type,
        env::var(LOG_ZEBRAD_CHECKPOINTS).is_ok(),
    )?;

    tracing::info!("\n\nfinished generating zebra-checkpoints",);

    Ok(())
}

/// Spawns a `zebra-checkpoints` instance on `network`, connected to `zebrad_rpc_address`.
///
/// Returns:
/// - `Ok(zebra_checkpoints)` on success,
/// - `Err(_)` if spawning `zebra-checkpoints` fails.
#[tracing::instrument]
pub fn spawn_zebra_checkpoints_direct(
    network: Network,
    test_type: TestType,
    zebrad_rpc_address: SocketAddr,
) -> Result<TestChild<TempDir>> {
    let arguments = args![
        "--addr": zebrad_rpc_address.to_string(),
        // TODO: get checkpoints out of `zebrad` or git somehow
        //"--last-checkpoint": last_checkpoint,
    ];

    // TODO: add logs for different kinds of zebra_checkpoints failures
    let zebra_checkpoints_failure_messages = PROCESS_FAILURE_MESSAGES
        .iter()
        .chain(ZEBRA_FAILURE_MESSAGES)
        .chain(ZEBRA_CHECKPOINTS_FAILURE_MESSAGES)
        .cloned();
    let zebra_checkpoints_ignore_messages = NO_MATCHES_REGEX_ITER.iter().cloned();

    // Currently unused, but we might put a copy of the checkpoints file in it later
    let zebra_checkpoints_dir = testdir()?;

    let mut zebra_checkpoints = zebra_checkpoints_dir
        .spawn_zebra_checkpoints_child(arguments)?
        .with_timeout(test_type.zebrad_timeout())
        .with_failure_regex_iter(
            zebra_checkpoints_failure_messages,
            zebra_checkpoints_ignore_messages,
        );

    // zebra-checkpoints does not log anything when it launches, it just prints checkpoints.
    // Mainnet and testnet checkpoints always have at least one leading zero in their hash.
    //
    // This log happens very quickly, so it is ok to block for a short while here.
    zebra_checkpoints.expect_stdout_line_matches(regex::escape(" 0"))?;

    Ok(zebra_checkpoints)
}

/// Extension trait for methods on `tempfile::TempDir` for using it as a test
/// directory for `zebra-checkpoints`.
pub trait ZebraCheckpointsTestDirExt: ZebradTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    /// Spawn `zebra-checkpoints` with `extra_args`, as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the child process.
    ///
    /// By default, launch an instance that connects directly to `zebrad`.
    fn spawn_zebra_checkpoints_child(self, extra_args: Arguments) -> Result<TestChild<Self>>;
}

impl<T> ZebraCheckpointsTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    #[allow(clippy::unwrap_in_result)]
    fn spawn_zebra_checkpoints_child(self, extra_args: Arguments) -> Result<TestChild<Self>> {
        // By default, launch an instance that connects directly to `zebrad`.
        let mut args = Arguments::new();
        args.set_parameter("--transport", "direct");

        // apply user provided arguments
        args.merge_with(extra_args);

        self.spawn_child_with_command("zebra-checkpoints", args)
    }
}

/// Wait for `zebra-checkpoints` to generate checkpoints, clearing Zebra's logs at the same time.
#[tracing::instrument]
pub fn wait_for_zebra_checkpoints_generation<
    P: ZebradTestDirExt + std::fmt::Debug + std::marker::Send + 'static,
>(
    mut zebra_checkpoints: TestChild<TempDir>,
    mut zebrad: TestChild<P>,
    zebra_tip_height: Height,
    test_type: TestType,
    show_zebrad_logs: bool,
) -> Result<(TestChild<TempDir>, TestChild<P>)> {
    let last_checkpoint_gap = HeightDiff::try_from(MIN_TRANSPARENT_COINBASE_MATURITY)
        .expect("constant fits in HeightDiff")
        + HeightDiff::try_from(MAX_CHECKPOINT_HEIGHT_GAP).expect("constant fits in HeightDiff");
    let expected_final_checkpoint_height =
        (zebra_tip_height - last_checkpoint_gap).expect("network tip is high enough");

    let is_zebra_checkpoints_finished = AtomicBool::new(false);
    let is_zebra_checkpoints_finished = &is_zebra_checkpoints_finished;

    // Check Zebra's logs for errors.
    //
    // Checkpoint generation can take a long time, so we need to check `zebrad` for errors
    // in parallel.
    let zebrad_mut = &mut zebrad;
    let zebrad_wait_fn = || -> Result<_> {
        tracing::debug!(
            ?test_type,
            "zebrad is waiting for zebra-checkpoints to generate checkpoints..."
        );
        while !is_zebra_checkpoints_finished.load(Ordering::SeqCst) {
            // Just keep checking the Zebra logs for errors...
            // Make sure the sync is still finished, this is logged every minute or so.
            zebrad_mut.expect_stdout_line_matches(crate::common::sync::SYNC_FINISHED_REGEX)?;
        }

        Ok(zebrad_mut)
    };

    // Wait until `zebra-checkpoints` has generated a full set of checkpoints.
    // Also checks `zebra-checkpoints` logs for errors.
    //
    // Checkpoints generation can take a long time, so we need to run it in parallel with `zebrad`.
    let zebra_checkpoints_mut = &mut zebra_checkpoints;
    let zebra_checkpoints_wait_fn = || -> Result<_> {
        tracing::debug!(
            ?test_type,
            "waiting for zebra_checkpoints to generate checkpoints..."
        );

        // zebra-checkpoints does not log anything when it finishes, it just prints checkpoints.
        //
        // We know that checkpoints are always less than 1000 blocks apart, but they can happen
        // anywhere in that range due to block sizes. So we ignore the last 3 digits of the height.
        let expected_final_checkpoint_prefix = expected_final_checkpoint_height.0 / 1000;

        // Mainnet and testnet checkpoints always have at least one leading zero in their hash.
        let expected_final_checkpoint =
            format!("{expected_final_checkpoint_prefix}[0-9][0-9][0-9] 0");
        zebra_checkpoints_mut.expect_stdout_line_matches(&expected_final_checkpoint)?;

        // Tell the other thread that `zebra_checkpoints` has finished
        is_zebra_checkpoints_finished.store(true, Ordering::SeqCst);

        Ok(zebra_checkpoints_mut)
    };

    // Run both threads in parallel, automatically propagating any panics to this thread.
    std::thread::scope(|s| {
        // Launch the sync-waiting threads
        let zebrad_thread = s.spawn(|| {
            zebrad_wait_fn().expect("test failed while waiting for zebrad to sync");
        });

        let zebra_checkpoints_thread = s.spawn(|| {
            zebra_checkpoints_wait_fn()
                .expect("test failed while waiting for zebra_checkpoints to sync.");
        });

        // Mark the sync-waiting threads as finished if they fail or panic.
        // This tells the other thread that it can exit.
        //
        // TODO: use `panic::catch_unwind()` instead,
        //       when `&mut zebra_test::command::TestChild<TempDir>` is unwind-safe
        s.spawn(|| {
            let zebrad_result = zebrad_thread.join();
            zebrad_result.expect("test panicked or failed while waiting for zebrad to sync");
        });
        s.spawn(|| {
            let zebra_checkpoints_result = zebra_checkpoints_thread.join();
            is_zebra_checkpoints_finished.store(true, Ordering::SeqCst);

            zebra_checkpoints_result
                .expect("test panicked or failed while waiting for zebra_checkpoints to sync");
        });
    });

    Ok((zebra_checkpoints, zebrad))
}

/// Returns an approximate `zebrad` tip height, using JSON-RPC.
#[tracing::instrument]
pub async fn zebrad_tip_height(zebra_rpc_address: SocketAddr) -> Result<Height> {
    let client = RpcRequestClient::new(zebra_rpc_address);

    let zebrad_blockchain_info = client
        .text_from_call("getblockchaininfo", "[]".to_string())
        .await?;
    let zebrad_blockchain_info: serde_json::Value = serde_json::from_str(&zebrad_blockchain_info)?;

    let zebrad_tip_height = zebrad_blockchain_info["result"]["blocks"]
        .try_into_height()
        .expect("unexpected block height: invalid Height value");

    Ok(zebrad_tip_height)
}
