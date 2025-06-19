//! Provides TestType enum with shared code for acceptance tests

use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use color_eyre::eyre::Result;
use indexmap::IndexSet;

use zebra_chain::{
    common::default_cache_dir,
    parameters::Network::{self, Mainnet},
};
use zebra_network::CacheDir;
use zebra_test::{command::NO_MATCHES_REGEX_ITER, prelude::*};
use zebrad::config::ZebradConfig;

use super::{
    config::{default_test_config, random_known_rpc_port_config},
    failure_messages::{
        LIGHTWALLETD_EMPTY_ZEBRA_STATE_IGNORE_MESSAGES, LIGHTWALLETD_FAILURE_MESSAGES,
        PROCESS_FAILURE_MESSAGES, ZEBRA_FAILURE_MESSAGES,
    },
    launch::{LIGHTWALLETD_DELAY, LIGHTWALLETD_FULL_SYNC_TIP_DELAY, LIGHTWALLETD_UPDATE_TIP_DELAY},
    lightwalletd::LWD_CACHE_DIR,
    sync::FINISH_PARTIAL_SYNC_TIMEOUT,
};

use TestType::*;

/// The type of integration test that we're running.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TestType {
    /// Launch with an empty Zebra and lightwalletd state.
    LaunchWithEmptyState {
        /// Configures whether the test uses lightwalletd.
        launches_lightwalletd: bool,
    },

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

    /// Launch with a Zebra and lightwalletd state that might or might not be empty.
    UseAnyState,

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
    #[allow(dead_code)]
    UpdateZebraCachedStateWithRpc,
}

impl TestType {
    /// Does this test need a Zebra cached state?
    pub fn needs_zebra_cached_state(&self) -> bool {
        // Handle the Zebra state directory based on the test type:
        // - LaunchWithEmptyState: ignore the state directory
        // - FullSyncFromGenesis, UpdateCachedState, UpdateZebraCachedStateNoRpc:
        //   skip the test if it is not available
        match self {
            LaunchWithEmptyState { .. } | UseAnyState => false,
            FullSyncFromGenesis { .. }
            | UpdateCachedState
            | UpdateZebraCachedStateNoRpc
            | UpdateZebraCachedStateWithRpc => true,
        }
    }

    /// Does this test need a Zebra rpc server?
    pub fn needs_zebra_rpc_server(&self) -> bool {
        match self {
            UpdateZebraCachedStateWithRpc | LaunchWithEmptyState { .. } => true,
            UseAnyState
            | UpdateZebraCachedStateNoRpc
            | FullSyncFromGenesis { .. }
            | UpdateCachedState => self.launches_lightwalletd(),
        }
    }

    /// Does this test launch `lightwalletd`?
    pub fn launches_lightwalletd(&self) -> bool {
        match self {
            UseAnyState | UpdateZebraCachedStateNoRpc | UpdateZebraCachedStateWithRpc => false,
            FullSyncFromGenesis { .. } | UpdateCachedState => true,
            LaunchWithEmptyState {
                launches_lightwalletd,
            } => *launches_lightwalletd,
        }
    }

    /// Does this test need a `lightwalletd` cached state?
    pub fn needs_lightwalletd_cached_state(&self) -> bool {
        // Handle the lightwalletd state directory based on the test type:
        // - LaunchWithEmptyState, UpdateZebraCachedStateNoRpc: ignore the state directory
        // - FullSyncFromGenesis: use it if available, timeout if it is already populated
        // - UpdateCachedState: skip the test if it is not available
        match self {
            LaunchWithEmptyState { .. }
            | UseAnyState
            | FullSyncFromGenesis { .. }
            | UpdateZebraCachedStateNoRpc
            | UpdateZebraCachedStateWithRpc => false,
            UpdateCachedState => true,
        }
    }

    /// Does this test allow a `lightwalletd` cached state, even if it is not required?
    pub fn allow_lightwalletd_cached_state(&self) -> bool {
        match self {
            LaunchWithEmptyState { .. } => false,
            FullSyncFromGenesis {
                allow_lightwalletd_cached_state,
            } => *allow_lightwalletd_cached_state,
            UseAnyState
            | UpdateCachedState
            | UpdateZebraCachedStateNoRpc
            | UpdateZebraCachedStateWithRpc => true,
        }
    }

    /// Can this test create a new `LWD_CACHE_DIR` cached state?
    pub fn can_create_lightwalletd_cached_state(&self) -> bool {
        match self {
            LaunchWithEmptyState { .. } | UseAnyState => false,
            FullSyncFromGenesis { .. } | UpdateCachedState => true,
            UpdateZebraCachedStateNoRpc | UpdateZebraCachedStateWithRpc => false,
        }
    }

    /// Returns the Zebra state path for this test, if configured and needed.
    #[allow(clippy::print_stderr)]
    pub fn zebrad_state_path<S: AsRef<str>>(&self, test_name: S) -> Option<PathBuf> {
        let test_name = test_name.as_ref();

        // Only return a path if this test type needs a cached state
        if !self.needs_zebra_cached_state() {
            return None;
        }

        // Create a config to check if a cache dir is configured
        let config = match self.zebrad_config(test_name, true, None, &Mainnet) {
            Some(Ok(config)) => config,
            Some(Err(_)) | None => {
                // Config creation failed or test should be skipped
                return None;
            }
        };

        // Check if a non-default cache dir is configured
        if config.state.cache_dir != default_cache_dir() {
            Some(config.state.cache_dir)
        } else {
            eprintln!(
                "skipped {test_name:?} {self:?} test, \
                 set the ZEBRA_STATE__CACHE_DIR environment variable to run the test",
            );
            None
        }
    }

    /// Returns a Zebra config for this test.
    ///
    /// `replace_cache_dir` replaces any cached or ephemeral state.
    ///
    /// Returns `None` if the test should be skipped,
    /// and `Some(Err(_))` if the config could not be created.
    pub fn zebrad_config<Str: AsRef<str>>(
        &self,
        test_name: Str,
        use_internet_connection: bool,
        replace_cache_dir: Option<&Path>,
        network: &Network,
    ) -> Option<Result<ZebradConfig>> {
        let config = if self.needs_zebra_rpc_server() {
            // This is what we recommend our users configure.
            random_known_rpc_port_config(true, network)
        } else {
            default_test_config(network)
        };

        let mut config = match config {
            Ok(config) => config,
            Err(error) => return Some(Err(error)),
        };

        // We want to run multi-threaded RPCs, if we're using them
        if self.launches_lightwalletd() {
            // Automatically runs one thread per available CPU core
            config.rpc.parallel_cpu_threads = 0;
        }

        if !use_internet_connection {
            config.network.initial_mainnet_peers = IndexSet::new();
            config.network.initial_testnet_peers = IndexSet::new();
            // Avoid reusing cached peers from disk when we're supposed to be a disconnected instance
            config.network.cache_dir = CacheDir::disabled();

            // Activate the mempool immediately by default
            config.mempool.debug_enable_at_height = Some(0);
        }

        // If we have a cached state, or we don't want to be ephemeral, update the config to use it
        if replace_cache_dir.is_some() || self.needs_zebra_cached_state() {
            let zebra_state_path = replace_cache_dir
                .map(|path| path.to_owned())
                .or_else(|| self.zebrad_state_path(test_name))?;

            config.state.ephemeral = false;
            config.state.cache_dir = zebra_state_path;

            // And reset the concurrency to the default value
            config.sync.checkpoint_verify_concurrency_limit =
                zebrad::components::sync::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;
        }

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
                 ignoring any cached state in the {LWD_CACHE_DIR:?} environment variable",
            );

            return None;
        }

        match env::var_os(LWD_CACHE_DIR) {
            Some(path) => Some(path.into()),
            None => {
                if self.needs_lightwalletd_cached_state() {
                    tracing::info!(
                        "skipped {test_name:?} {self:?} lightwalletd test, \
                         set the {LWD_CACHE_DIR:?} environment variable to run the test",
                    );
                } else if self.allow_lightwalletd_cached_state() {
                    tracing::info!(
                        "running {test_name:?} {self:?} lightwalletd test without cached state, \
                         set the {LWD_CACHE_DIR:?} environment variable to run with cached state",
                    );
                }

                None
            }
        }
    }

    /// Returns the `zebrad` timeout for this test type.
    pub fn zebrad_timeout(&self) -> Duration {
        match self {
            LaunchWithEmptyState { .. } | UseAnyState => LIGHTWALLETD_DELAY,
            FullSyncFromGenesis { .. } => LIGHTWALLETD_FULL_SYNC_TIP_DELAY,
            UpdateCachedState | UpdateZebraCachedStateNoRpc => LIGHTWALLETD_UPDATE_TIP_DELAY,
            UpdateZebraCachedStateWithRpc => FINISH_PARTIAL_SYNC_TIMEOUT,
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
            LaunchWithEmptyState { .. } | UseAnyState => LIGHTWALLETD_DELAY,
            FullSyncFromGenesis { .. } => LIGHTWALLETD_FULL_SYNC_TIP_DELAY,
            UpdateCachedState | UpdateZebraCachedStateNoRpc | UpdateZebraCachedStateWithRpc => {
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
        if matches!(*self, LaunchWithEmptyState { .. }) {
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
            lightwalletd_failure_messages.push("no chain tip available yet".to_string());
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

        let lightwalletd_ignore_messages = if matches!(*self, LaunchWithEmptyState { .. }) {
            LIGHTWALLETD_EMPTY_ZEBRA_STATE_IGNORE_MESSAGES.iter()
        } else {
            NO_MATCHES_REGEX_ITER.iter()
        }
        .map(ToString::to_string)
        .collect();

        (lightwalletd_failure_messages, lightwalletd_ignore_messages)
    }
}
