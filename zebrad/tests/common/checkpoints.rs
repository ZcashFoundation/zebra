//! Test generating checkpoints using `zebra-checkpoints` directly connected to `zebrad`.
//!
//! This test requires a cached chain state that is synchronized close to the network chain tip
//! height. It will finish the sync and update the cached chain state.

use std::{
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
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
use zebra_state::state_database_format_version_in_code;
use zebra_test::{
    args,
    command::{Arguments, TestDirExt, NO_MATCHES_REGEX_ITER},
    prelude::TestChild,
};

use crate::common::{
    cached_state::{
        wait_for_state_version_message, wait_for_state_version_upgrade,
        DATABASE_FORMAT_UPGRADE_IS_SLOWER_THAN_RPC_SPAWN,
    },
    launch::spawn_zebrad_for_rpc,
    sync::{CHECKPOINT_VERIFIER_REGEX, SYNC_FINISHED_REGEX},
    test_type::TestType::*,
};

use super::{
    config::testdir,
    failure_messages::{
        PROCESS_FAILURE_MESSAGES, ZEBRA_CHECKPOINTS_FAILURE_MESSAGES, ZEBRA_FAILURE_MESSAGES,
    },
    launch::ZebradTestDirExt,
    test_type::TestType,
};

/// The environmental variable used to activate zebrad logs in the checkpoint generation test.
///
/// We use a constant so the compiler detects typos.
pub const LOG_ZEBRAD_CHECKPOINTS: &str = "LOG_ZEBRAD_CHECKPOINTS";

/// The test entry point.
#[allow(clippy::print_stdout)]
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
        spawn_zebrad_for_rpc(network.clone(), test_name, test_type, true)?
    {
        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Wait for the upgrade if needed.
    // Currently we only write an image for testnet, which is quick.
    // (Mainnet would need to wait at the end of this function, if the upgrade is long.)
    if network.is_a_test_network() {
        let state_version_message = wait_for_state_version_message(&mut zebrad)?;

        // Before we write a cached state image, wait for a database upgrade.
        //
        // It is ok if the logs are in the wrong order and the test sometimes fails,
        // because testnet is unreliable anyway.
        //
        // TODO: this line will hang if the state upgrade is slower than the RPC server spawn.
        // But that is unlikely, because both 25.1 and 25.2 are quick on testnet.
        //
        // TODO: combine this check with the CHECKPOINT_VERIFIER_REGEX and RPC endpoint checks.
        // This is tricky because we need to get the last checkpoint log.
        wait_for_state_version_upgrade(
            &mut zebrad,
            &state_version_message,
            state_database_format_version_in_code(),
            None,
        )?;
    }

    let zebra_rpc_address = zebra_rpc_address.expect("zebra_checkpoints test must have RPC port");

    tracing::info!(
        ?network,
        ?zebra_rpc_address,
        "spawned zebrad, waiting for it to load compiled-in checkpoints...",
    );

    let last_checkpoint = zebrad.expect_stdout_line_matches(CHECKPOINT_VERIFIER_REGEX)?;

    // TODO: do this with a regex?
    let (_prefix, last_checkpoint) = last_checkpoint
        .split_once("max_checkpoint_height")
        .expect("just checked log format");
    let (_prefix, last_checkpoint) = last_checkpoint
        .split_once('(')
        .expect("unexpected log format");
    let (last_checkpoint, _suffix) = last_checkpoint
        .split_once(')')
        .expect("unexpected log format");

    tracing::info!(
        ?network,
        ?zebra_rpc_address,
        ?last_checkpoint,
        "found zebrad's current last checkpoint",
    );

    tracing::info!(
        ?network,
        ?zebra_rpc_address,
        "waiting for zebrad to open its RPC port...",
    );
    zebrad.expect_stdout_line_matches(format!("Opened RPC endpoint at {zebra_rpc_address}"))?;

    tracing::info!(
        ?network,
        ?zebra_rpc_address,
        "zebrad opened its RPC port, waiting for it to sync...",
    );

    zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

    let zebra_tip_height = zebrad_tip_height(zebra_rpc_address).await?;
    tracing::info!(
        ?network,
        ?zebra_rpc_address,
        ?zebra_tip_height,
        ?last_checkpoint,
        "zebrad synced to the tip, launching zebra-checkpoints...",
    );

    let zebra_checkpoints = spawn_zebra_checkpoints_direct(
        network.clone(),
        test_type,
        zebra_rpc_address,
        last_checkpoint,
    )?;

    let show_zebrad_logs = env::var(LOG_ZEBRAD_CHECKPOINTS).is_ok();
    if !show_zebrad_logs {
        tracing::info!(
            "zebrad logs are hidden, show them using {LOG_ZEBRAD_CHECKPOINTS}=1 and RUST_LOG=debug"
        );
    }

    tracing::info!(
        ?network,
        ?zebra_rpc_address,
        ?zebra_tip_height,
        ?last_checkpoint,
        "spawned zebra-checkpoints connected to zebrad, checkpoints should appear here...",
    );
    println!("\n\n");

    let (_zebra_checkpoints, _zebrad) = wait_for_zebra_checkpoints_generation(
        zebra_checkpoints,
        zebrad,
        zebra_tip_height,
        test_type,
        show_zebrad_logs,
    )?;

    println!("\n\n");
    tracing::info!(
        ?network,
        ?zebra_tip_height,
        ?last_checkpoint,
        "finished generating Zebra checkpoints",
    );

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
    last_checkpoint: &str,
) -> Result<TestChild<TempDir>> {
    let zebrad_rpc_address = zebrad_rpc_address.to_string();

    let arguments = args![
        "--addr": zebrad_rpc_address,
        "--last-checkpoint": last_checkpoint,
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

    // zebra-checkpoints logs to stderr when it launches.
    //
    // This log happens very quickly, so it is ok to block for a short while here.
    zebra_checkpoints.expect_stderr_line_matches(regex::escape("calculating checkpoints"))?;

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

impl ZebraCheckpointsTestDirExt for TempDir {
    #[allow(clippy::unwrap_in_result)]
    fn spawn_zebra_checkpoints_child(mut self, extra_args: Arguments) -> Result<TestChild<Self>> {
        // By default, launch an instance that connects directly to `zebrad`.
        let mut args = Arguments::new();
        args.set_parameter("--transport", "direct");

        // Apply user provided arguments
        args.merge_with(extra_args);

        // Create debugging info
        let temp_dir = self.as_ref().display().to_string();

        // Try searching the system $PATH first, that's what the test Docker image uses
        let zebra_checkpoints_path = "zebra-checkpoints";

        // Make sure we have the right zebra-checkpoints binary.
        //
        // When we were creating this test, we spent a lot of time debugging a build issue where
        // `zebra-checkpoints` had an empty `main()` function. This check makes sure that doesn't
        // happen again.
        let debug_checkpoints = env::var(LOG_ZEBRAD_CHECKPOINTS).is_ok();
        if debug_checkpoints {
            let mut args = Arguments::new();
            args.set_argument("--help");

            let help_dir = testdir()?;

            tracing::info!(
                ?zebra_checkpoints_path,
                ?args,
                ?help_dir,
                system_path = ?env::var("PATH"),
                // TODO: disable when the tests are working well
                usr_local_zebra_checkpoints_info = ?fs::metadata("/usr/local/bin/zebra-checkpoints"),
                "Trying to launch `zebra-checkpoints --help` by searching system $PATH...",
            );

            let zebra_checkpoints = help_dir.spawn_child_with_command(zebra_checkpoints_path, args);

            if let Err(help_error) = zebra_checkpoints {
                tracing::info!(?help_error, "Failed to launch `zebra-checkpoints --help`");
            } else {
                tracing::info!("Launched `zebra-checkpoints --help`, output is:");

                let mut zebra_checkpoints = zebra_checkpoints.unwrap();
                let mut output_is_empty = true;

                // Get the help output
                while zebra_checkpoints.wait_for_stdout_line(None) {
                    output_is_empty = false;
                }
                while zebra_checkpoints.wait_for_stderr_line(None) {
                    output_is_empty = false;
                }

                if output_is_empty {
                    tracing::info!(
                        "`zebra-checkpoints --help` did not log any output. \
                         Is the binary being built during tests? Are its required-features active?"
                    );
                }
            }
        }

        // Try the `zebra-checkpoints` binary the Docker image copied just after it built the tests.
        tracing::info!(
            ?zebra_checkpoints_path,
            ?args,
            ?temp_dir,
            system_path = ?env::var("PATH"),
            // TODO: disable when the tests are working well
            usr_local_zebra_checkpoints_info = ?fs::metadata("/usr/local/bin/zebra-checkpoints"),
            "Trying to launch zebra-checkpoints by searching system $PATH...",
        );

        let zebra_checkpoints = self.spawn_child_with_command(zebra_checkpoints_path, args.clone());

        let Err(system_path_error) = zebra_checkpoints else {
            return zebra_checkpoints;
        };

        // Fall back to assuming zebra-checkpoints is in the same directory as zebrad.
        let mut zebra_checkpoints_path: PathBuf = env!("CARGO_BIN_EXE_zebrad").into();
        assert!(
            zebra_checkpoints_path.pop(),
            "must have at least one path component",
        );
        zebra_checkpoints_path.push("zebra-checkpoints");

        if zebra_checkpoints_path.exists() {
            // Create a new temporary directory, because the old one has been used up.
            //
            // TODO: instead, return the TempDir from spawn_child_with_command() on error.
            self = testdir()?;

            // Create debugging info
            let temp_dir = self.as_ref().display().to_string();

            tracing::info!(
                ?zebra_checkpoints_path,
                ?args,
                ?temp_dir,
                ?system_path_error,
                // TODO: disable when the tests are working well
                zebra_checkpoints_info = ?fs::metadata(&zebra_checkpoints_path),
                "Launching from system $PATH failed, \
                 trying to launch zebra-checkpoints from cargo path...",
            );

            self.spawn_child_with_command(
                zebra_checkpoints_path.to_str().expect(
                    "internal test harness error: path is not UTF-8 \
                     TODO: change spawn child methods to take &OsStr not &str",
                ),
                args,
            )
        } else {
            tracing::info!(
                cargo_path = ?zebra_checkpoints_path,
                ?system_path_error,
                // TODO: disable when the tests are working well
                cargo_path_info = ?fs::metadata(&zebra_checkpoints_path),
                "Launching from system $PATH failed, \
                 and zebra-checkpoints cargo path does not exist...",
            );

            // Return the original error
            Err(system_path_error)
        }
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
    let last_checkpoint_gap = HeightDiff::from(MIN_TRANSPARENT_COINBASE_MATURITY)
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
            "zebrad is waiting for zebra-checkpoints to generate checkpoints...",
        );
        while !is_zebra_checkpoints_finished.load(Ordering::SeqCst) {
            // Just keep silently checking the Zebra logs for errors,
            // so the checkpoint list can be copied from the output.
            //
            // Make sure the sync is still finished, this is logged every minute or so.
            if env::var(LOG_ZEBRAD_CHECKPOINTS).is_ok() {
                zebrad_mut.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;
            } else {
                zebrad_mut.expect_stdout_line_matches_silent(SYNC_FINISHED_REGEX)?;
            }
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
            "waiting for zebra_checkpoints to generate checkpoints...",
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

        // Write the rest of the checkpoints: there can be 0-2 more checkpoints.
        while zebra_checkpoints_mut.wait_for_stdout_line(None) {}

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
            let zebra_checkpoints_result = zebra_checkpoints_wait_fn();

            is_zebra_checkpoints_finished.store(true, Ordering::SeqCst);

            zebra_checkpoints_result
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
