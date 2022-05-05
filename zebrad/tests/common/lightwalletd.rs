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

use zebra_test::{
    command::{Arguments, TestChild, TestDirExt, NO_MATCHES_REGEX_ITER},
    net::random_known_port,
    prelude::*,
};
use zebrad::config::ZebradConfig;

use super::{
    cached_state::ZEBRA_CACHED_STATE_DIR_VAR,
    config::default_test_config,
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

pub mod send_transaction_test;
pub mod wallet_grpc;
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
pub const LIGHTWALLETD_DATA_DIR_VAR: &str = "LIGHTWALLETD_DATA_DIR";

/// The maximum time that a `lightwalletd` integration test is expected to run.
pub const LIGHTWALLETD_TEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Should we skip Zebra lightwalletd integration tests?
#[allow(clippy::print_stderr)]
pub fn zebra_skip_lightwalletd_tests() -> bool {
    // TODO: check if the lightwalletd binary is in the PATH?
    //       (this doesn't seem to be implemented in the standard library)
    //
    // See is_command_available in zebra-test/tests/command.rs for one way to do this.

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
pub fn random_known_rpc_port_config() -> Result<ZebradConfig> {
    // [Note on port conflict](#Note on port conflict)
    let listen_port = random_known_port();
    let listen_ip = "127.0.0.1".parse().expect("hard-coded IP is valid");
    let zebra_rpc_listener = SocketAddr::new(listen_ip, listen_port);

    // Write a configuration that has the rpc listen_addr option set
    // TODO: split this config into another function?
    let mut config = default_test_config()?;
    config.rpc.listen_addr = Some(zebra_rpc_listener);

    Ok(config)
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
}

impl LightwalletdTestType {
    /// Does this test need a Zebra cached state?
    pub fn needs_zebra_cached_state(&self) -> bool {
        match self {
            LaunchWithEmptyState => false,
            FullSyncFromGenesis { .. } | UpdateCachedState => true,
        }
    }

    /// Does this test need a lightwalletd cached state?
    pub fn needs_lightwalletd_cached_state(&self) -> bool {
        match self {
            LaunchWithEmptyState | FullSyncFromGenesis { .. } => false,
            UpdateCachedState => true,
        }
    }

    /// Does this test allow a lightwalletd cached state, even if it is not required?
    pub fn allow_lightwalletd_cached_state(&self) -> bool {
        match self {
            LaunchWithEmptyState => false,
            FullSyncFromGenesis {
                allow_lightwalletd_cached_state,
            } => *allow_lightwalletd_cached_state,
            UpdateCachedState => true,
        }
    }

    /// Returns the Zebra state path for this test, if set.
    pub fn zebrad_state_path(&self) -> Option<PathBuf> {
        match env::var_os(ZEBRA_CACHED_STATE_DIR_VAR) {
            Some(path) => Some(path.into()),
            None => {
                tracing::info!(
                    "skipped {self:?} lightwalletd test, \
                     set the {ZEBRA_CACHED_STATE_DIR_VAR:?} environment variable to run the test",
                );

                None
            }
        }
    }

    /// Returns a Zebra config for this test.
    ///
    /// Returns `None` if the test should be skipped,
    /// and `Some(Err(_))` if the config could not be created.
    pub fn zebrad_config(&self) -> Option<Result<ZebradConfig>> {
        if !self.needs_zebra_cached_state() {
            return Some(random_known_rpc_port_config());
        }

        let zebra_state_path = self.zebrad_state_path()?;

        let mut config = match random_known_rpc_port_config() {
            Ok(config) => config,
            Err(error) => return Some(Err(error)),
        };

        config.sync.lookahead_limit = zebrad::components::sync::DEFAULT_LOOKAHEAD_LIMIT;

        config.state.ephemeral = false;
        config.state.cache_dir = zebra_state_path;

        Some(Ok(config))
    }

    /// Returns the lightwalletd state path for this test, if set.
    pub fn lightwalletd_state_path(&self) -> Option<PathBuf> {
        env::var_os(LIGHTWALLETD_DATA_DIR_VAR).map(Into::into)
    }

    /// Returns the `zebrad` timeout for this test type.
    pub fn zebrad_timeout(&self) -> Duration {
        match self {
            LaunchWithEmptyState => LIGHTWALLETD_DELAY,
            FullSyncFromGenesis { .. } | UpdateCachedState => LIGHTWALLETD_UPDATE_TIP_DELAY,
        }
    }

    /// Returns the `lightwalletd` timeout for this test type.
    pub fn lightwalletd_timeout(&self) -> Duration {
        match self {
            LaunchWithEmptyState => LIGHTWALLETD_DELAY,
            UpdateCachedState => LIGHTWALLETD_UPDATE_TIP_DELAY,
            FullSyncFromGenesis { .. } => LIGHTWALLETD_FULL_SYNC_TIP_DELAY,
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
            zebrad_failure_messages.push("loaded Zebra state cache tip=None".to_string());
        }
        if *self == LaunchWithEmptyState {
            // Fail if we need an empty Zebra state, but it has blocks
            zebrad_failure_messages
                .push(r"loaded Zebra state cache tip=.*Height\([1-9][0-9]*\)".to_string());
        }

        let zebrad_ignore_messages = Vec::new();

        (zebrad_failure_messages, zebrad_ignore_messages)
    }

    /// Returns `lightwalletd` log regexes that indicate the tests have failed,
    /// and regexes of any failures that should be ignored.
    pub fn lightwalletd_failure_messages(&self) -> (Vec<String>, Vec<String>) {
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
            //
            // TODO: fail on `[0-9]{1,6}` when we're using the tip cached state (#4155)
            lightwalletd_failure_messages.push("Found [0-9]{1,5} blocks in cache".to_string());
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
