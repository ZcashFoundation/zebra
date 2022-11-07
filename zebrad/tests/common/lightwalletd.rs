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
    time::Duration,
};

use tempfile::TempDir;

use zebra_chain::parameters::Network::{self, *};
use zebra_test::{
    args,
    command::{Arguments, TestChild, TestDirExt, NO_MATCHES_REGEX_ITER},
    net::random_known_port,
    prelude::*,
};
use zebrad::config::ZebradConfig;

use super::{
    cached_state::ZEBRA_CACHED_STATE_DIR,
    config::{default_test_config, testdir},
    failure_messages::{
        LIGHTWALLETD_EMPTY_ZEBRA_STATE_IGNORE_MESSAGES, LIGHTWALLETD_FAILURE_MESSAGES,
        PROCESS_FAILURE_MESSAGES, ZEBRA_FAILURE_MESSAGES,
    },
    launch::{
        ZebradTestDirExt, LIGHTWALLETD_DELAY, LIGHTWALLETD_FULL_SYNC_TIP_DELAY,
        LIGHTWALLETD_UPDATE_TIP_DELAY,
    },
};

use LightwalletdTestType::*;

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
pub const ZEBRA_TEST_LIGHTWALLETD: &str = "ZEBRA_TEST_LIGHTWALLETD";

/// Optional environment variable with the cached state for lightwalletd.
///
/// Required for [`LightwalletdTestType::UpdateCachedState`],
/// so we can test lightwalletd RPC integration with a populated state.
///
/// Can also be used to speed up the [`sending_transactions_using_lightwalletd`] test,
/// by skipping the lightwalletd initial sync.
pub const LIGHTWALLETD_DATA_DIR: &str = "LIGHTWALLETD_DATA_DIR";

/// Should we skip Zebra lightwalletd integration tests?
#[allow(clippy::print_stderr)]
pub fn zebra_skip_lightwalletd_tests() -> bool {
    // TODO: check if the lightwalletd binary is in the PATH?
    //       (this doesn't seem to be implemented in the standard library)
    //
    // See is_command_available() in zebra-test/src/tests/command.rs for one way to do this.

    if env::var_os(ZEBRA_TEST_LIGHTWALLETD).is_none() {
        // This message is captured by the test runner, use
        // `cargo test -- --nocapture` to see it.
        eprintln!(
            "Skipped lightwalletd integration test, \
             set the 'ZEBRA_TEST_LIGHTWALLETD' environmental variable to run the test",
        );
        return true;
    }

    false
}

/// Returns a `zebrad` config with a random known RPC port.
///
/// Set `parallel_cpu_threads` to true to auto-configure based on the number of CPU cores.
pub fn random_known_rpc_port_config(parallel_cpu_threads: bool) -> Result<ZebradConfig> {
    // [Note on port conflict](#Note on port conflict)
    let listen_port = random_known_port();
    let listen_ip = "127.0.0.1".parse().expect("hard-coded IP is valid");
    let zebra_rpc_listener = SocketAddr::new(listen_ip, listen_port);

    // Write a configuration that has the rpc listen_addr option set
    // TODO: split this config into another function?
    let mut config = default_test_config()?;
    config.rpc.listen_addr = Some(zebra_rpc_listener);
    if parallel_cpu_threads {
        // Auto-configure to the number of CPU cores: most users configre this
        config.rpc.parallel_cpu_threads = 0;
    } else {
        // Default config, users who want to detect port conflicts configure this
        config.rpc.parallel_cpu_threads = 1;
    }

    Ok(config)
}

/// Spawns a lightwalletd instance on `network`, connected to `zebrad_rpc_address`,
/// with its gRPC server functionality enabled.
///
/// Expects cached state based on the `test_type`. Use the `LIGHTWALLETD_DATA_DIR`
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
    test_type: LightwalletdTestType,
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
        .spawn_lightwalletd_child(lightwalletd_state_path, arguments)?
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
    test_type: LightwalletdTestType,
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
/// directory for `zebrad`.
pub trait LightWalletdTestDirExt: ZebradTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    /// Spawn `lightwalletd` with `lightwalletd_state_path`, and `extra_args`,
    /// as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the child process.
    ///
    /// By default, launch a working test instance with logging, and avoid port conflicts.
    ///
    /// # Panics
    ///
    /// If there is no lightwalletd config in the test directory.
    fn spawn_lightwalletd_child(
        self,
        lightwalletd_state_path: impl Into<Option<PathBuf>>,
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
            args.set_parameter(
                "--data-dir",
                lightwalletd_state_path
                    .to_str()
                    .expect("path is valid Unicode"),
            );
        } else {
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

        let lightwalletd_config = format!(
            "\
            rpcbind={}\n\
            rpcport={}\n\
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

/// The type of lightwalletd integration test that we're running.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LightwalletdTestType {
    /// Launch with an empty Zebra and lightwalletd state.
    LaunchWithEmptyState,

    /// Do a full sync from an empty lightwalletd state.
    ///
    /// This test requires a cached Zebra state.
    //
    // Only used with `--features=lightwalletd-grpc-tests`.
    #[allow(dead_code)]
    FullSyncFromGenesis {
        /// Should the test allow a cached lightwalletd state?
        ///
        /// If `false`, the test fails if the lightwalletd state is populated.
        allow_lightwalletd_cached_state: bool,
    },

    /// Sync to tip from a lightwalletd cached state.
    ///
    /// This test requires a cached Zebra and lightwalletd state.
    UpdateCachedState,

    /// Launch `zebrad` and sync it to the tip, but don't launch `lightwalletd`.
    ///
    /// If this test fails, the failure is in `zebrad` without RPCs or `lightwalletd`.
    /// If it succeeds, but the RPC tests fail, the problem is caused by RPCs or `lightwalletd`.
    ///
    /// This test requires a cached Zebra state.
    UpdateZebraCachedStateNoRpc,

    /// Launch `zebrad` and sync it to the tip, but don't launch `lightwalletd`.
    ///
    /// This test requires a cached Zebra state.
    UpdateZebraCachedState,
}

impl LightwalletdTestType {
    /// Does this test need a Zebra cached state?
    pub fn needs_zebra_cached_state(&self) -> bool {
        // Handle the Zebra state directory based on the test type:
        // - LaunchWithEmptyState: ignore the state directory
        // - FullSyncFromGenesis, UpdateCachedState, UpdateZebraCachedStateNoRpc:
        //   skip the test if it is not available
        match self {
            LaunchWithEmptyState => false,
            FullSyncFromGenesis { .. }
            | UpdateCachedState
            | UpdateZebraCachedStateNoRpc
            | UpdateZebraCachedState => true,
        }
    }

    /// Does this test launch `lightwalletd`?
    pub fn launches_lightwalletd(&self) -> bool {
        match self {
            UpdateZebraCachedStateNoRpc | UpdateZebraCachedState => false,
            LaunchWithEmptyState | FullSyncFromGenesis { .. } | UpdateCachedState => true,
        }
    }

    /// Does this test need a `lightwalletd` cached state?
    pub fn needs_lightwalletd_cached_state(&self) -> bool {
        // Handle the lightwalletd state directory based on the test type:
        // - LaunchWithEmptyState, UpdateZebraCachedStateNoRpc: ignore the state directory
        // - FullSyncFromGenesis: use it if available, timeout if it is already populated
        // - UpdateCachedState: skip the test if it is not available
        match self {
            LaunchWithEmptyState
            | FullSyncFromGenesis { .. }
            | UpdateZebraCachedStateNoRpc
            | UpdateZebraCachedState => false,
            UpdateCachedState => true,
        }
    }

    /// Does this test allow a `lightwalletd` cached state, even if it is not required?
    pub fn allow_lightwalletd_cached_state(&self) -> bool {
        match self {
            LaunchWithEmptyState => false,
            FullSyncFromGenesis {
                allow_lightwalletd_cached_state,
            } => *allow_lightwalletd_cached_state,
            UpdateCachedState | UpdateZebraCachedStateNoRpc | UpdateZebraCachedState => true,
        }
    }

    /// Can this test create a new `LIGHTWALLETD_DATA_DIR` cached state?
    pub fn can_create_lightwalletd_cached_state(&self) -> bool {
        match self {
            LaunchWithEmptyState => false,
            FullSyncFromGenesis { .. } | UpdateCachedState => true,
            UpdateZebraCachedStateNoRpc | UpdateZebraCachedState => false,
        }
    }

    /// Returns the Zebra state path for this test, if set.
    #[allow(clippy::print_stderr)]
    pub fn zebrad_state_path<S: AsRef<str>>(&self, test_name: S) -> Option<PathBuf> {
        match env::var_os(ZEBRA_CACHED_STATE_DIR) {
            Some(path) => Some(path.into()),
            None => {
                let test_name = test_name.as_ref();
                eprintln!(
                    "skipped {test_name:?} {self:?} lightwalletd test, \
                     set the {ZEBRA_CACHED_STATE_DIR:?} environment variable to run the test",
                );

                None
            }
        }
    }

    /// Returns a Zebra config for this test.
    ///
    /// Returns `None` if the test should be skipped,
    /// and `Some(Err(_))` if the config could not be created.
    pub fn zebrad_config<S: AsRef<str>>(&self, test_name: S) -> Option<Result<ZebradConfig>> {
        let config = if self.launches_lightwalletd() {
            // This is what we recommend our users configure.
            random_known_rpc_port_config(true)
        } else {
            default_test_config()
        };

        let mut config = match config {
            Ok(config) => config,
            Err(error) => return Some(Err(error)),
        };

        // We want to preload the consensus parameters,
        // except when we're doing the quick empty state test
        config.consensus.debug_skip_parameter_preload = !self.needs_zebra_cached_state();

        // We want to run multi-threaded RPCs, if we're using them
        if self.launches_lightwalletd() {
            // Automatically runs one thread per available CPU core
            config.rpc.parallel_cpu_threads = 0;
        }

        if !self.needs_zebra_cached_state() {
            return Some(Ok(config));
        }

        let zebra_state_path = self.zebrad_state_path(test_name)?;

        config.sync.checkpoint_verify_concurrency_limit =
            zebrad::components::sync::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;

        config.state.ephemeral = false;
        config.state.cache_dir = zebra_state_path;

        Some(Ok(config))
    }

    /// Returns the `lightwalletd` state path for this test, if set, and if allowed for this test.
    pub fn lightwalletd_state_path<S: AsRef<str>>(&self, test_name: S) -> Option<PathBuf> {
        let test_name = test_name.as_ref();

        // Can this test type use a lwd cached state, or create/update one?
        let use_or_create_lwd_cache =
            self.allow_lightwalletd_cached_state() || self.can_create_lightwalletd_cached_state();

        if !self.launches_lightwalletd() || !use_or_create_lwd_cache {
            tracing::info!(
                "running {test_name:?} {self:?} lightwalletd test, \
                 ignoring any cached state in the {LIGHTWALLETD_DATA_DIR:?} environment variable",
            );

            return None;
        }

        match env::var_os(LIGHTWALLETD_DATA_DIR) {
            Some(path) => Some(path.into()),
            None => {
                if self.needs_lightwalletd_cached_state() {
                    tracing::info!(
                        "skipped {test_name:?} {self:?} lightwalletd test, \
                         set the {LIGHTWALLETD_DATA_DIR:?} environment variable to run the test",
                    );
                } else if self.allow_lightwalletd_cached_state() {
                    tracing::info!(
                        "running {test_name:?} {self:?} lightwalletd test without cached state, \
                         set the {LIGHTWALLETD_DATA_DIR:?} environment variable to run with cached state",
                    );
                }

                None
            }
        }
    }

    /// Returns the `zebrad` timeout for this test type.
    pub fn zebrad_timeout(&self) -> Duration {
        match self {
            LaunchWithEmptyState => LIGHTWALLETD_DELAY,
            FullSyncFromGenesis { .. } => LIGHTWALLETD_FULL_SYNC_TIP_DELAY,
            UpdateCachedState | UpdateZebraCachedStateNoRpc | UpdateZebraCachedState => {
                LIGHTWALLETD_UPDATE_TIP_DELAY
            }
        }
    }

    /// Returns the `lightwalletd` timeout for this test type.
    #[track_caller]
    pub fn lightwalletd_timeout(&self) -> Duration {
        if !self.launches_lightwalletd() {
            panic!("lightwalletd must not be launched in the {self:?} test");
        }

        // We use the same timeouts for zebrad and lightwalletd,
        // because the tests check zebrad and lightwalletd concurrently.
        match self {
            LaunchWithEmptyState => LIGHTWALLETD_DELAY,
            FullSyncFromGenesis { .. } => LIGHTWALLETD_FULL_SYNC_TIP_DELAY,
            UpdateCachedState | UpdateZebraCachedStateNoRpc | UpdateZebraCachedState => {
                LIGHTWALLETD_UPDATE_TIP_DELAY
            }
        }
    }

    /// Returns Zebra log regexes that indicate the tests have failed,
    /// and regexes of any failures that should be ignored.
    pub fn zebrad_failure_messages(&self) -> (Vec<String>, Vec<String>) {
        let mut zebrad_failure_messages: Vec<String> = ZEBRA_FAILURE_MESSAGES
            .iter()
            .chain(PROCESS_FAILURE_MESSAGES)
            .map(ToString::to_string)
            .collect();

        if self.needs_zebra_cached_state() {
            // Fail if we need a cached Zebra state, but it's empty
            zebrad_failure_messages.push("loaded Zebra state cache .*tip.*=.*None".to_string());
        }
        if *self == LaunchWithEmptyState {
            // Fail if we need an empty Zebra state, but it has blocks
            zebrad_failure_messages
                .push(r"loaded Zebra state cache .*tip.*=.*Height\([1-9][0-9]*\)".to_string());
        }

        let zebrad_ignore_messages = Vec::new();

        (zebrad_failure_messages, zebrad_ignore_messages)
    }

    /// Returns `lightwalletd` log regexes that indicate the tests have failed,
    /// and regexes of any failures that should be ignored.
    #[track_caller]
    pub fn lightwalletd_failure_messages(&self) -> (Vec<String>, Vec<String>) {
        if !self.launches_lightwalletd() {
            panic!("lightwalletd must not be launched in the {self:?} test");
        }

        let mut lightwalletd_failure_messages: Vec<String> = LIGHTWALLETD_FAILURE_MESSAGES
            .iter()
            .chain(PROCESS_FAILURE_MESSAGES)
            .map(ToString::to_string)
            .collect();

        // Zebra state failures
        if self.needs_zebra_cached_state() {
            // Fail if we need a cached Zebra state, but it's empty
            lightwalletd_failure_messages.push("No Chain tip available yet".to_string());
        }

        // lightwalletd state failures
        if self.needs_lightwalletd_cached_state() {
            // Fail if we need a cached lightwalletd state, but it isn't near the tip
            lightwalletd_failure_messages.push("Found [0-9]{1,6} blocks in cache".to_string());
        }
        if !self.allow_lightwalletd_cached_state() {
            // Fail if we need an empty lightwalletd state, but it has blocks
            lightwalletd_failure_messages.push("Found [1-9][0-9]* blocks in cache".to_string());
        }

        let lightwalletd_ignore_messages = if *self == LaunchWithEmptyState {
            LIGHTWALLETD_EMPTY_ZEBRA_STATE_IGNORE_MESSAGES.iter()
        } else {
            NO_MATCHES_REGEX_ITER.iter()
        }
        .map(ToString::to_string)
        .collect();

        (lightwalletd_failure_messages, lightwalletd_ignore_messages)
    }
}
