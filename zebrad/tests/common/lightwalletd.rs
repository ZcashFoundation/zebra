//! `lightwalletd`-specific shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

use std::{
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use zebra_chain::parameters::Network::{self, *};
use zebra_test::{
    args,
    command::{Arguments, TestDirExt},
    net::random_known_port,
    prelude::*,
};

use color_eyre::eyre::WrapErr;
use zebra_state::state_database_format_version_in_code;

use super::{
    cached_state::{
        wait_for_state_version_message, wait_for_state_version_upgrade,
        DATABASE_FORMAT_UPGRADE_IS_LONG,
    },
    config::testdir,
    launch::{spawn_zebrad_for_rpc, ZebradTestDirExt},
    sync::SYNC_FINISHED_REGEX,
    test_type::TestType,
};

#[cfg(feature = "lightwalletd-grpc-tests")]
pub mod send_transaction_test;
#[cfg(feature = "lightwalletd-grpc-tests")]
pub mod sync;
#[cfg(feature = "lightwalletd-grpc-tests")]
pub mod wallet_grpc;
#[cfg(feature = "lightwalletd-grpc-tests")]
pub mod wallet_grpc_test;

/// The name of the env var that enables Zebra lightwalletd integration tests.
/// These tests need a `lightwalletd` binary in the test machine's path.
///
/// We use a constant so that the compiler detects typos.
///
/// # Note
///
/// This environmental variable is used to enable the lightwalletd tests.
/// But the network tests are *disabled* by their environmental variables.
pub const TEST_LIGHTWALLETD: &str = "TEST_LIGHTWALLETD";

/// Optional environment variable that points to a cached lightwalletd state directory.
///
/// - If unset, tests look for a platform-specific default (for example, `~/.cache/lwd` on Linux).
/// - Tests that require a populated lightwalletd cache (currently [`TestType::UpdateCachedState`])
///   will be skipped if neither an override nor an existing default cache is found.
/// - Providing a populated cache can significantly speed up certain tests (for example,
///   the sending-transactions-via-lightwalletd test), by avoiding the initial lightwalletd sync.
pub const LWD_CACHE_DIR: &str = "LWD_CACHE_DIR";

/// Should we skip Zebra lightwalletd integration tests?
#[allow(clippy::print_stderr)]
pub fn zebra_skip_lightwalletd_tests() -> bool {
    // TODO: check if the lightwalletd binary is in the PATH?
    //       (this doesn't seem to be implemented in the standard library)
    //
    // See is_command_available() in zebra-test/src/tests/command.rs for one way to do this.

    if env::var_os(TEST_LIGHTWALLETD).is_none() {
        // This message is captured by the test runner, use
        // `cargo test -- --nocapture` to see it.
        eprintln!(
            "Skipped lightwalletd integration test, \
             set the 'TEST_LIGHTWALLETD' environmental variable to run the test",
        );
        return true;
    }

    false
}

/// Spawns a lightwalletd instance on `network`, connected to `zebrad_rpc_address`,
/// with its gRPC server functionality enabled.
///
/// Expects cached state based on the `test_type`. Use the `LWD_CACHE_DIR`
/// environmental variable to provide an initial state to the lightwalletd instance.
///
/// Returns:
/// - `Ok(Some(lightwalletd, lightwalletd_rpc_port))` on success,
/// - `Ok(None)` if the test doesn't have the required network or cached state, and
/// - `Err(_)` if spawning lightwalletd fails.
#[tracing::instrument]
pub fn spawn_lightwalletd_for_rpc<S: AsRef<str> + std::fmt::Debug>(
    network: Network,
    test_name: S,
    test_type: TestType,
    zebrad_rpc_address: SocketAddr,
) -> Result<Option<(TestChild<TempDir>, u16)>> {
    assert_eq!(network, Mainnet, "this test only supports Mainnet for now");

    let test_name = test_name.as_ref();

    // Skip the test unless the user specifically asked for it
    if !can_spawn_lightwalletd_for_rpc(test_name, test_type) {
        return Ok(None);
    }

    let lightwalletd_state_path = test_type.lightwalletd_state_path(test_name);
    let lightwalletd_dir = testdir()?.with_lightwalletd_config(zebrad_rpc_address)?;

    let lightwalletd_rpc_port = random_known_port();
    let lightwalletd_rpc_address = format!("127.0.0.1:{lightwalletd_rpc_port}");

    let arguments = args!["--grpc-bind-addr": lightwalletd_rpc_address];

    let (lightwalletd_failure_messages, lightwalletd_ignore_messages) =
        test_type.lightwalletd_failure_messages();

    let mut lightwalletd = lightwalletd_dir
        .spawn_lightwalletd_child(lightwalletd_state_path, test_type, arguments)?
        .with_timeout(test_type.lightwalletd_timeout())
        .with_failure_regex_iter(lightwalletd_failure_messages, lightwalletd_ignore_messages);

    // Wait until `lightwalletd` has launched.
    // This log happens very quickly, so it is ok to block for a short while here.
    lightwalletd.expect_stdout_line_matches(regex::escape("Starting gRPC server"))?;

    Ok(Some((lightwalletd, lightwalletd_rpc_port)))
}

/// Returns `true` if a lightwalletd test for `test_type` has everything it needs to run.
#[tracing::instrument]
pub fn can_spawn_lightwalletd_for_rpc<S: AsRef<str> + std::fmt::Debug>(
    test_name: S,
    test_type: TestType,
) -> bool {
    if zebra_test::net::zebra_skip_network_tests() {
        return false;
    }

    // Skip the test unless the user specifically asked for it
    //
    // TODO: pass test_type to zebra_skip_lightwalletd_tests() and check for lightwalletd launch in there
    if test_type.launches_lightwalletd() && zebra_skip_lightwalletd_tests() {
        return false;
    }

    let lightwalletd_state_path = test_type.lightwalletd_state_path(test_name);
    if test_type.needs_lightwalletd_cached_state() && lightwalletd_state_path.is_none() {
        return false;
    }

    true
}

/// Extension trait for methods on `tempfile::TempDir` for using it as a test
/// directory for `lightwalletd`.
pub trait LightWalletdTestDirExt: ZebradTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    /// Spawn `lightwalletd` with `lightwalletd_state_path`, and `extra_args`,
    /// as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the child process.
    ///
    /// Uses `test_type` to determine logging behavior for the state directory.
    ///
    /// By default, launch a working test instance with logging, and avoid port conflicts.
    ///
    /// # Panics
    ///
    /// If there is no lightwalletd config in the test directory.
    fn spawn_lightwalletd_child(
        self,
        lightwalletd_state_path: impl Into<Option<PathBuf>>,
        test_type: TestType,
        extra_args: Arguments,
    ) -> Result<TestChild<Self>>;

    /// Create a config file and use it for all subsequently spawned `lightwalletd` processes.
    /// Returns an error if the config already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config
    fn with_lightwalletd_config(self, zebra_rpc_listener: SocketAddr) -> Result<Self>;
}

impl<T> LightWalletdTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    #[allow(clippy::unwrap_in_result)]
    fn spawn_lightwalletd_child(
        self,
        lightwalletd_state_path: impl Into<Option<PathBuf>>,
        test_type: TestType,
        extra_args: Arguments,
    ) -> Result<TestChild<Self>> {
        let test_dir = self.as_ref().to_owned();
        let default_config_path = test_dir.join("lightwalletd-zcash.conf");

        assert!(
            default_config_path.exists(),
            "lightwalletd requires a config"
        );

        // By default, launch a working test instance with logging,
        // and avoid port conflicts.
        let mut args = Arguments::new();

        // the fake zcashd conf we just wrote
        let zcash_conf_path = default_config_path
            .as_path()
            .to_str()
            .expect("Path is valid Unicode");
        args.set_parameter("--zcash-conf-path", zcash_conf_path);

        // the lightwalletd cache directory
        if let Some(lightwalletd_state_path) = lightwalletd_state_path.into() {
            tracing::info!(?lightwalletd_state_path, "using lightwalletd state path");

            // Only log the directory size if it's expected to exist already.
            // FullSyncFromGenesis creates this directory, so we skip logging for it.
            if !matches!(test_type, TestType::FullSyncFromGenesis { .. }) {
                let lwd_cache_dir_path = lightwalletd_state_path.join("db/main");
                let lwd_cache_entries: Vec<_> = std::fs::read_dir(&lwd_cache_dir_path)
                    .unwrap_or_else(|error| {
                        if error.kind() == std::io::ErrorKind::NotFound {
                            panic!(
                                "missing cached lightwalletd state at {path:?}.\n\
                                 Populate the directory (for example by running the lwd_sync_full \n\
                                 stateful test) or set {env_var} to a populated cache.",
                                path = lwd_cache_dir_path,
                                env_var = LWD_CACHE_DIR,
                            );
                        }

                        panic!(
                            "unexpected failure opening lightwalletd cache dir {path:?}: {error:?}",
                            path = lwd_cache_dir_path,
                            error = error,
                        );
                    })
                    .collect();

                let lwd_cache_dir_size = lwd_cache_entries.iter().fold(0, |acc, entry_result| {
                    acc + entry_result
                        .as_ref()
                        .map(|entry| entry.metadata().map(|meta| meta.len()).unwrap_or(0))
                        .unwrap_or(0)
                });

                tracing::info!("{lwd_cache_dir_size} bytes in lightwalletd cache dir");

                for entry_result in &lwd_cache_entries {
                    match entry_result {
                        Ok(entry) => tracing::info!("{entry:?} entry in lightwalletd cache dir"),
                        Err(e) => {
                            tracing::warn!(?e, "error reading entry in lightwalletd cache dir")
                        }
                    }
                }
            }

            args.set_parameter(
                "--data-dir",
                lightwalletd_state_path
                    .to_str()
                    .expect("path is valid Unicode"),
            );
        } else {
            tracing::info!("using lightwalletd empty state path");
            let empty_state_path = test_dir.join("lightwalletd_state");

            std::fs::create_dir(&empty_state_path)
                .expect("unexpected failure creating lightwalletd state sub-directory");

            args.set_parameter(
                "--data-dir",
                empty_state_path.to_str().expect("path is valid Unicode"),
            );
        }

        // log to standard output
        //
        // TODO: if lightwalletd needs to run on Windows,
        //       work out how to log to the terminal on all platforms
        args.set_parameter("--log-file", "/dev/stdout");

        // let the OS choose a random available wallet client port
        args.set_parameter("--grpc-bind-addr", "127.0.0.1:0");
        args.set_parameter("--http-bind-addr", "127.0.0.1:0");

        // don't require a TLS certificate for the HTTP server
        args.set_argument("--no-tls-very-insecure");

        // apply user provided arguments
        args.merge_with(extra_args);

        self.spawn_child_with_command("lightwalletd", args)
    }

    fn with_lightwalletd_config(self, zebra_rpc_listener: SocketAddr) -> Result<Self> {
        use std::fs;

        // zcash/lightwalletd requires rpcuser and rpcpassword, or a zcashd cookie file
        // But when a lightwalletd with this config is used by Zebra,
        // Zebra ignores any authentication and provides access regardless.
        let lightwalletd_config = format!(
            "\
            rpcbind={}\n\
            rpcport={}\n\
            rpcuser=xxxxx
            rpcpassword=xxxxx
            ",
            zebra_rpc_listener.ip(),
            zebra_rpc_listener.port(),
        );

        let dir = self.as_ref();
        fs::create_dir_all(dir)?;

        let config_file = dir.join("lightwalletd-zcash.conf");
        fs::write(config_file, lightwalletd_config.as_bytes())?;

        Ok(self)
    }
}

/// Run a lightwalletd integration test with a configuration for `test_type`.
///
/// Tests that sync `lightwalletd` to the chain tip require the `lightwalletd-grpc-tests` feature`:
/// - [`FullSyncFromGenesis`]
/// - [`UpdateCachedState`]
///
/// Set `FullSyncFromGenesis { allow_lightwalletd_cached_state: true }` to speed up manual full sync tests.
///
/// # Reliability
///
/// The random ports in this test can cause [rare port conflicts.](#Note on port conflict)
///
/// # Panics
///
/// If the `test_type` requires `--features=lightwalletd-grpc-tests`,
/// but Zebra was not compiled with that feature.
#[tracing::instrument]
pub fn lwd_integration_test(test_type: TestType) -> Result<()> {
    let _init_guard = zebra_test::init();

    // We run these sync tests with a network connection, for better test coverage.
    let use_internet_connection = true;
    let network = Mainnet;
    let test_name = "lwd_integration_test";

    if test_type.launches_lightwalletd() && !can_spawn_lightwalletd_for_rpc(test_name, test_type) {
        tracing::info!("skipping test due to missing lightwalletd network or cached state");
        return Ok(());
    }

    // Launch zebra with peers and using a predefined zebrad state path.
    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) = spawn_zebrad_for_rpc(
        network.clone(),
        test_name,
        test_type,
        use_internet_connection,
    )? {
        tracing::info!(
            ?test_type,
            "running lightwalletd & zebrad integration test, launching zebrad...",
        );

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Store the state version message so we can wait for the upgrade later if needed.
    let state_version_message = wait_for_state_version_message(&mut zebrad)?;

    if test_type.needs_zebra_cached_state() {
        zebrad
            .expect_stdout_line_matches(r"loaded Zebra state cache .*tip.*=.*Height\([0-9]{7}\)")?;
    } else {
        // Timeout the test if we're somehow accidentally using a cached state
        zebrad.expect_stdout_line_matches("loaded Zebra state cache .*tip.*=.*None")?;
    }

    // Wait for the state to upgrade and the RPC port, if the upgrade is short.
    //
    // If incompletely upgraded states get written to the CI cache,
    // change DATABASE_FORMAT_UPGRADE_IS_LONG to true.
    if !DATABASE_FORMAT_UPGRADE_IS_LONG {
        if test_type.launches_lightwalletd() {
            tracing::info!(
                ?test_type,
                ?zebra_rpc_address,
                "waiting for zebrad to open its RPC port..."
            );
            wait_for_state_version_upgrade(
                &mut zebrad,
                &state_version_message,
                state_database_format_version_in_code(),
                [format!(
                    "Opened RPC endpoint at {}",
                    zebra_rpc_address.expect("lightwalletd test must have RPC port")
                )],
            )?;
        } else {
            wait_for_state_version_upgrade(
                &mut zebrad,
                &state_version_message,
                state_database_format_version_in_code(),
                None,
            )?;
        }
    }

    // Wait for zebrad to sync the genesis block before launching lightwalletd,
    // if lightwalletd is launched and zebrad starts with an empty state.
    // This prevents lightwalletd from exiting early due to an empty state.
    if test_type.launches_lightwalletd() && !test_type.needs_zebra_cached_state() {
        tracing::info!(
            ?test_type,
            "waiting for zebrad to sync genesis block before launching lightwalletd...",
        );
        // Wait for zebrad to commit the genesis block to the state.
        // Use the syncer's state tip log message, as the specific commit log might not appear reliably.
        zebrad.expect_stdout_line_matches(
            "starting sync, obtaining new tips state_tip=Some\\(Height\\(0\\)\\)",
        )?;
    }

    // Launch lightwalletd, if needed
    let lightwalletd_and_port = if test_type.launches_lightwalletd() {
        tracing::info!(
            ?zebra_rpc_address,
            "launching lightwalletd connected to zebrad",
        );

        // Launch lightwalletd
        let (mut lightwalletd, lightwalletd_rpc_port) = spawn_lightwalletd_for_rpc(
            network,
            test_name,
            test_type,
            zebra_rpc_address.expect("lightwalletd test must have RPC port"),
        )?
        .expect("already checked for lightwalletd cached state and network");

        tracing::info!(
            ?lightwalletd_rpc_port,
            "spawned lightwalletd connected to zebrad",
        );

        // Check that `lightwalletd` is calling the expected Zebra RPCs

        // getblockchaininfo
        if test_type.needs_zebra_cached_state() {
            lightwalletd.expect_stdout_line_matches(
                "Got sapling height 419200 block height [0-9]{7} chain main branchID [0-9a-f]{8}",
            )?;
        } else {
            // Timeout the test if we're somehow accidentally using a cached state in our temp dir
            lightwalletd.expect_stdout_line_matches(regex::escape(
                "Got sapling height 419200 block height [0-9]{1,6} chain main branchID 00000000",
            ))?;
        }

        if test_type.needs_lightwalletd_cached_state() {
            lightwalletd
                .expect_stdout_line_matches("Done reading [0-9]{7} blocks from disk cache")?;
        } else if !test_type.allow_lightwalletd_cached_state() {
            // Timeout the test if we're somehow accidentally using a cached state in our temp dir
            lightwalletd.expect_stdout_line_matches("Done reading 0 blocks from disk cache")?;
        }

        // getblock with the first Sapling block in Zebra's state
        //
        // zcash/lightwalletd calls getbestblockhash here, but
        // adityapk00/lightwalletd calls getblock
        //
        // The log also depends on what is in Zebra's state:
        //
        // # Cached Zebra State
        //
        // lightwalletd ingests blocks into its cache.
        //
        // # Empty Zebra State
        //
        // lightwalletd tries to download the Sapling activation block, but it's not in the state.
        //
        // Until the Sapling activation block has been downloaded,
        // lightwalletd will keep retrying getblock.
        if !test_type.allow_lightwalletd_cached_state() {
            if test_type.needs_zebra_cached_state() {
                lightwalletd.expect_stdout_line_matches(
                    "([Aa]dding block to cache)|([Ww]aiting for block)",
                )?;
            } else {
                lightwalletd.expect_stdout_line_matches(regex::escape(
                    "Waiting for zcashd height to reach Sapling activation height (419200)",
                ))?;
            }
        }

        Some((lightwalletd, lightwalletd_rpc_port))
    } else {
        None
    };

    // Wait for zebrad and lightwalletd to sync, if needed.
    let (mut zebrad, lightwalletd) = if test_type.needs_zebra_cached_state() {
        if let Some((lightwalletd, lightwalletd_rpc_port)) = lightwalletd_and_port {
            #[cfg(feature = "lightwalletd-grpc-tests")]
            {
                use super::lightwalletd::sync::wait_for_zebrad_and_lightwalletd_sync;

                tracing::info!(
                    ?lightwalletd_rpc_port,
                    "waiting for zebrad and lightwalletd to sync...",
                );

                let (lightwalletd, mut zebrad) = wait_for_zebrad_and_lightwalletd_sync(
                    lightwalletd,
                    lightwalletd_rpc_port,
                    zebrad,
                    zebra_rpc_address.expect("lightwalletd test must have RPC port"),
                    test_type,
                    // We want to wait for the mempool and network for better coverage
                    true,
                    use_internet_connection,
                )?;

                // Wait for the state to upgrade, if the upgrade is long.
                // If this line hangs, change DATABASE_FORMAT_UPGRADE_IS_LONG to false,
                // or combine "wait for sync" with "wait for state version upgrade".
                if DATABASE_FORMAT_UPGRADE_IS_LONG {
                    wait_for_state_version_upgrade(
                        &mut zebrad,
                        &state_version_message,
                        state_database_format_version_in_code(),
                        None,
                    )?;
                }

                (zebrad, Some(lightwalletd))
            }

            #[cfg(not(feature = "lightwalletd-grpc-tests"))]
            panic!(
                "the {test_type:?} test requires `cargo test --feature lightwalletd-grpc-tests`\n\
                 zebrad: {zebrad:?}\n\
                 lightwalletd: {lightwalletd:?}\n\
                 lightwalletd_rpc_port: {lightwalletd_rpc_port:?}"
            );
        } else {
            // We're just syncing Zebra, so there's no lightwalletd to check
            tracing::info!(?test_type, "waiting for zebrad to sync to the tip");
            zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

            // Wait for the state to upgrade, if the upgrade is long.
            // If this line hangs, change DATABASE_FORMAT_UPGRADE_IS_LONG to false.
            if DATABASE_FORMAT_UPGRADE_IS_LONG {
                wait_for_state_version_upgrade(
                    &mut zebrad,
                    &state_version_message,
                    state_database_format_version_in_code(),
                    None,
                )?;
            }

            (zebrad, None)
        }
    } else {
        let lightwalletd = lightwalletd_and_port.map(|(lightwalletd, _port)| lightwalletd);

        // We don't have a cached state, so we don't do any tip checks for Zebra or lightwalletd
        (zebrad, lightwalletd)
    };

    tracing::info!(
        ?test_type,
        "cleaning up child processes and checking for errors",
    );

    // Cleanup both processes
    //
    // If the test fails here, see the [note on port conflict](#Note on port conflict)
    //
    // zcash/lightwalletd exits by itself, but
    // adityapk00/lightwalletd keeps on going, so it gets killed by the test harness.
    zebrad.kill(false)?;

    if let Some(mut lightwalletd) = lightwalletd {
        lightwalletd.kill(false)?;

        let lightwalletd_output = lightwalletd.wait_with_output()?.assert_failure()?;

        lightwalletd_output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    }

    let zebrad_output = zebrad.wait_with_output()?.assert_failure()?;

    zebrad_output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}
