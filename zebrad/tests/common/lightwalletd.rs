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

use super::{config::testdir, launch::ZebradTestDirExt, test_type::TestType};

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

/// Optional environment variable with the cached state for lightwalletd.
///
/// Required for [`TestType::UpdateCachedState`],
/// so we can test lightwalletd RPC integration with a populated state.
///
/// Can also be used to speed up the [`sending_transactions_using_lightwalletd`] test,
/// by skipping the lightwalletd initial sync.
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
                    .expect("unexpected failure reading lightwalletd cache dir")
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
