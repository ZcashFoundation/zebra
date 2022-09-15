//! `zebrad` launch-specific shared code for the `zebrad` acceptance tests.
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

use color_eyre::eyre::Result;
use indexmap::IndexSet;
use tempfile::TempDir;

use zebra_chain::parameters::Network;
use zebra_test::{
    args,
    command::{Arguments, TestDirExt},
    prelude::*,
};
use zebrad::config::ZebradConfig;

use crate::common::{
    lightwalletd::{random_known_rpc_port_config, LightwalletdTestType},
    sync::FINISH_PARTIAL_SYNC_TIMEOUT,
};

/// After we launch `zebrad`, wait this long for the command to start up,
/// take the actions expected by the tests, and log the expected logs.
///
/// Previously, this value was 3 seconds, which caused rare
/// metrics or tracing test failures in Windows CI.
pub const LAUNCH_DELAY: Duration = Duration::from_secs(15);

/// After we launch `lightwalletd`, wait this long for the command to start up,
/// take the actions expected by the quick tests, and log the expected logs.
///
/// `lightwalletd`'s actions also depend on the actions of the `zebrad` instance
/// it is using for its RPCs.
pub const LIGHTWALLETD_DELAY: Duration = Duration::from_secs(60);

/// The amount of time we wait between launching two conflicting nodes.
///
/// We use a longer time to make sure the first node has launched before the second starts,
/// even if CI is under load.
pub const BETWEEN_NODES_DELAY: Duration = Duration::from_secs(5);

/// The amount of time we wait for lightwalletd to update to the tip.
///
/// `lightwalletd` takes about 60-120 minutes to fully sync,
/// and `zebrad` can take hours to update to the tip under load.
pub const LIGHTWALLETD_UPDATE_TIP_DELAY: Duration = FINISH_PARTIAL_SYNC_TIMEOUT;

/// The amount of time we wait for lightwalletd to do a full sync to the tip.
///
/// See [`LIGHTWALLETD_UPDATE_TIP_DELAY`] for details.
pub const LIGHTWALLETD_FULL_SYNC_TIP_DELAY: Duration = FINISH_PARTIAL_SYNC_TIMEOUT;

/// Extension trait for methods on `tempfile::TempDir` for using it as a test
/// directory for `zebrad`.
pub trait ZebradTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    // Zebra methods

    /// Spawn `zebrad` with `args` as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the
    /// child process.
    ///
    /// If there is a config in the test directory, pass it to `zebrad`.
    fn spawn_child(self, args: Arguments) -> Result<TestChild<Self>>;

    /// Create a config file and use it for all subsequently spawned `zebrad` processes.
    /// Returns an error if the config already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    ///   - set `config.cache_dir` based on `self`
    fn with_config(self, config: &mut ZebradConfig) -> Result<Self>;

    /// Create a config file with the exact contents of `config`, and use it for
    /// all subsequently spawned `zebrad` processes. Returns an error if the config
    /// already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    fn with_exact_config(self, config: &ZebradConfig) -> Result<Self>;

    /// Overwrite any existing `zebrad` config file, and use the newly written config for
    /// all subsequently spawned processes.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state
    ///   - set `config.cache_dir` based on `self`
    fn replace_config(self, config: &mut ZebradConfig) -> Result<Self>;

    /// `cache_dir` config update helper for `zebrad`.
    ///
    /// If needed:
    ///   - set the cache_dir in the config.
    fn cache_config_update_helper(self, config: &mut ZebradConfig) -> Result<Self>;

    /// Config writing helper for `zebrad`.
    ///
    /// If needed:
    ///   - recursively create directories for the config and state,
    ///
    /// Then write out the config.
    fn write_config_helper(self, config: &ZebradConfig) -> Result<Self>;
}

impl<T> ZebradTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    #[allow(clippy::unwrap_in_result)]
    fn spawn_child(self, extra_args: Arguments) -> Result<TestChild<Self>> {
        let dir = self.as_ref();
        let default_config_path = dir.join("zebrad.toml");
        let mut args = Arguments::new();

        if default_config_path.exists() {
            args.set_parameter(
                "-c",
                default_config_path
                    .as_path()
                    .to_str()
                    .expect("Path is valid Unicode"),
            );
        }

        args.merge_with(extra_args);

        self.spawn_child_with_command(env!("CARGO_BIN_EXE_zebrad"), args)
    }

    fn with_config(self, config: &mut ZebradConfig) -> Result<Self> {
        self.cache_config_update_helper(config)?
            .write_config_helper(config)
    }

    fn with_exact_config(self, config: &ZebradConfig) -> Result<Self> {
        self.write_config_helper(config)
    }

    fn replace_config(self, config: &mut ZebradConfig) -> Result<Self> {
        use std::fs;
        use std::io::ErrorKind;

        // Remove any existing config before writing a new one
        let dir = self.as_ref();
        let config_file = dir.join("zebrad.toml");
        match fs::remove_file(config_file) {
            Ok(()) => {}
            // If the config file doesn't exist, that's ok
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => Err(e)?,
        }

        self.cache_config_update_helper(config)?
            .write_config_helper(config)
    }

    fn cache_config_update_helper(self, config: &mut ZebradConfig) -> Result<Self> {
        if !config.state.ephemeral {
            let dir = self.as_ref();
            let cache_dir = PathBuf::from(dir);
            config.state.cache_dir = cache_dir;
        }

        Ok(self)
    }

    fn write_config_helper(self, config: &ZebradConfig) -> Result<Self> {
        use std::fs;
        use std::io::Write;

        let dir = self.as_ref();

        if !config.state.ephemeral {
            let cache_dir = dir.join("state");
            fs::create_dir_all(&cache_dir)?;
        } else {
            fs::create_dir_all(dir)?;
        }

        let config_file = dir.join("zebrad.toml");
        fs::File::create(config_file)?.write_all(toml::to_string(&config)?.as_bytes())?;

        Ok(self)
    }
}

/// Spawns a zebrad instance to interact with lightwalletd.
///
/// If `internet_connection` is `false` then spawn, but without any peers.
/// This prevents it from downloading blocks. Instead, the `zebra_directory` parameter allows
/// providing an initial state to the zebrad instance.
#[tracing::instrument]
pub fn spawn_zebrad_for_rpc<P: ZebradTestDirExt + std::fmt::Debug>(
    network: Network,
    zebra_directory: P,
    test_type: LightwalletdTestType,
    debug_skip_parameter_preload: bool,
    internet_connection: bool,
) -> Result<(TestChild<P>, SocketAddr)> {
    // This is what we recommend our users configure.
    let mut config = random_known_rpc_port_config(true)
        .expect("Failed to create a config file with a known RPC listener port");

    config.state.ephemeral = false;
    if !internet_connection {
        config.network.initial_mainnet_peers = IndexSet::new();
        config.network.initial_testnet_peers = IndexSet::new();
    }
    config.network.network = network;
    if !internet_connection {
        config.mempool.debug_enable_at_height = Some(0);
    }
    config.consensus.debug_skip_parameter_preload = debug_skip_parameter_preload;

    let (zebrad_failure_messages, zebrad_ignore_messages) = test_type.zebrad_failure_messages();

    let mut zebrad = zebra_directory
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .bypass_test_capture(true)
        .with_timeout(test_type.zebrad_timeout())
        .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

    let rpc_address = config.rpc.listen_addr.unwrap();

    zebrad.expect_stdout_line_matches(&format!("Opened RPC endpoint at {}", rpc_address))?;

    Ok((zebrad, rpc_address))
}

/// Wait for lightwalletd to sync to Zebra's tip.
///
/// "Adding block" and "Waiting for block" logs stop when `lightwalletd` reaches the tip.
/// But if the logs just stop, we can't tell the difference between a hang and fully synced.
/// So we assume `lightwalletd` will sync and log large groups of blocks,
/// and check for logs with heights near the mainnet tip height.
#[tracing::instrument]
pub fn wait_for_zebrad_and_lightwalletd_sync<
    P: ZebradTestDirExt + std::fmt::Debug + std::marker::Send + 'static,
>(
    mut lightwalletd: TestChild<TempDir>,
    mut zebrad: TestChild<P>,
    test_type: LightwalletdTestType,
    wait_for_zebrad_tip: bool,
) -> Result<(TestChild<TempDir>, TestChild<P>)> {
    let lightwalletd_thread = std::thread::spawn(move || -> Result<_> {
        tracing::info!(?test_type, "waiting for lightwalletd to sync to the tip");

        lightwalletd
            .expect_stdout_line_matches(crate::common::sync::LIGHTWALLETD_SYNC_FINISHED_REGEX)?;

        Ok(lightwalletd)
    });

    // `lightwalletd` syncs can take a long time,
    // so we need to check that `zebrad` has synced to the tip in parallel.
    let lightwalletd_thread_and_zebrad = std::thread::spawn(move || -> Result<_> {
        tracing::info!(?test_type, "waiting for zebrad to sync to the tip");

        while !lightwalletd_thread.is_finished() {
            zebrad.expect_stdout_line_matches(crate::common::sync::SYNC_FINISHED_REGEX)?;
        }

        Ok((lightwalletd_thread, zebrad))
    });

    // Retrieve the child process handles from the threads
    let (lightwalletd_thread, mut zebrad) = lightwalletd_thread_and_zebrad
        .join()
        .unwrap_or_else(|panic_object| std::panic::resume_unwind(panic_object))?;

    let lightwalletd = lightwalletd_thread
        .join()
        .unwrap_or_else(|panic_object| std::panic::resume_unwind(panic_object))?;

    // If we are in sync mempool should activate
    zebrad.expect_stdout_line_matches("activating mempool")?;

    Ok((lightwalletd, zebrad))
}

/// Panics if `$pred` is false, with an error report containing:
///   * context from `$source`, and
///   * an optional wrapper error, using `$fmt_arg`+ as a format string and
///     arguments.
#[macro_export]
macro_rules! assert_with_context {
    ($pred:expr, $source:expr) => {
        if !$pred {
            use color_eyre::Section as _;
            use color_eyre::SectionExt as _;
            use zebra_test::command::ContextFrom as _;
            let report = color_eyre::eyre::eyre!("failed assertion")
                .section(stringify!($pred).header("Predicate:"))
                .context_from($source);

            panic!("Error: {:?}", report);
        }
    };
    ($pred:expr, $source:expr, $($fmt_arg:tt)+) => {
        if !$pred {
            use color_eyre::Section as _;
            use color_eyre::SectionExt as _;
            use zebra_test::command::ContextFrom as _;
            let report = color_eyre::eyre::eyre!("failed assertion")
                .section(stringify!($pred).header("Predicate:"))
                .context_from($source)
                .wrap_err(format!($($fmt_arg)+));

            panic!("Error: {:?}", report);
        }
    };
}
