//! `zebrad` launch-specific shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

use std::{
    env,
    fmt::Debug,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use tempfile::TempDir;

use zebra_chain::parameters::Network::{self, *};
use zebra_network::CacheDir;
use zebra_test::{
    args,
    command::{Arguments, TestDirExt},
    prelude::*,
};
use zebrad::config::ZebradConfig;

use crate::common::{
    config::testdir, lightwalletd::zebra_skip_lightwalletd_tests,
    sync::FINISH_PARTIAL_SYNC_TIMEOUT, test_type::TestType,
};

/// After we launch `zebrad`, wait this long for the command to start up,
/// take the actions expected by the tests, and log the expected logs.
///
/// Previously, this value was 3 seconds, which caused rare
/// metrics or tracing test failures in Windows CI.
pub const LAUNCH_DELAY: Duration = Duration::from_secs(20);

/// After we launch `zebrad`, wait this long in extended tests.
/// See [`LAUNCH_DELAY`] for details.
pub const EXTENDED_LAUNCH_DELAY: Duration = Duration::from_secs(45);

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
pub const BETWEEN_NODES_DELAY: Duration = Duration::from_secs(20);

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
        let dir = self.as_ref();
        let cache_dir = PathBuf::from(dir);

        // If the peer cache has already been disabled, don't re-enable it
        if config.network.cache_dir.is_enabled() {
            config.network.cache_dir = CacheDir::custom_path(&cache_dir);
        }

        // Only replace the state cache directory if it's going to be used
        if !config.state.ephemeral {
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
            fs::create_dir_all(cache_dir)?;
        } else {
            fs::create_dir_all(dir)?;
        }

        let config_file = dir.join("zebrad.toml");
        fs::File::create(config_file)?.write_all(toml::to_string(&config)?.as_bytes())?;

        Ok(self)
    }
}

/// Spawns a zebrad instance on `network` to test lightwalletd with `test_type`.
///
/// If `use_internet_connection` is `false` then spawn, but without any peers.
/// This prevents it from downloading blocks. Instead, use the `STATE_CACHE_DIR`
/// environmental variable to provide an initial state to the zebrad instance.
///
/// Returns:
/// - `Ok(Some(zebrad, zebra_rpc_address))` on success,
/// - `Ok(None)` if the test doesn't have the required network or cached state, and
/// - `Err(_)` if spawning zebrad fails.
///
/// `zebra_rpc_address` is `None` if the test type doesn't need an RPC port.
#[tracing::instrument]
pub fn spawn_zebrad_for_rpc<S: AsRef<str> + Debug>(
    network: Network,
    test_name: S,
    test_type: TestType,
    use_internet_connection: bool,
) -> Result<Option<(TestChild<TempDir>, Option<SocketAddr>)>> {
    let test_name = test_name.as_ref();

    // Skip the test unless the user specifically asked for it
    if !can_spawn_zebrad_for_test_type(test_name, test_type, use_internet_connection) {
        return Ok(None);
    }

    // Get the zebrad config
    let config = test_type
        .zebrad_config(test_name, use_internet_connection, None, &network)
        .expect("already checked config")?;

    let (zebrad_failure_messages, zebrad_ignore_messages) = test_type.zebrad_failure_messages();

    // Writes a configuration that has RPC listen_addr set (if needed).
    // If the state path env var is set, uses it in the config.
    let zebrad = testdir()?
        .with_exact_config(&config)?
        .spawn_child(args!["start"])?
        .bypass_test_capture(true)
        .with_timeout(test_type.zebrad_timeout())
        .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

    Ok(Some((zebrad, config.rpc.listen_addr)))
}

/// Spawns a zebrad instance on `network` without RPCs or `lightwalletd`.
///
/// If `use_cached_state` is `true`, then update the cached state to the tip.
/// If `ephemeral` is `true`, then use an ephemeral state path.
/// If `reuse_state_path` is `Some(path)`, then use the state at that path, and take ownership of
/// the temporary directory, so it isn't deleted until the test ends.
/// Otherwise, just create an empty state in this test's new temporary directory.
///
/// If `use_internet_connection` is `false` then spawn, but without any peers.
/// This prevents it from downloading blocks. Instead, use the `STATE_CACHE_DIR`
/// environmental variable to provide an initial state to the zebrad instance.
///
/// Returns:
/// - `Ok(Some(zebrad))` on success,
/// - `Ok(None)` if the test doesn't have the required network or cached state, and
/// - `Err(_)` if spawning zebrad fails.
#[tracing::instrument]
pub fn spawn_zebrad_without_rpc<Str, Dir>(
    network: Network,
    test_name: Str,
    use_cached_state: bool,
    ephemeral: bool,
    reuse_state_path: Dir,
    use_internet_connection: bool,
) -> Result<Option<TestChild<TempDir>>>
where
    Str: AsRef<str> + Debug,
    Dir: Into<Option<TempDir>> + Debug,
{
    use TestType::*;

    let test_name = test_name.as_ref();

    let reuse_state_path = reuse_state_path.into();
    let testdir = reuse_state_path
        .unwrap_or_else(|| testdir().expect("failed to create test temporary directory"));

    let (test_type, replace_cache_dir) = if use_cached_state {
        (UpdateZebraCachedStateNoRpc, None)
    } else if ephemeral {
        (
            LaunchWithEmptyState {
                launches_lightwalletd: false,
            },
            None,
        )
    } else {
        (UseAnyState, Some(testdir.path()))
    };

    // Skip the test unless the user specifically asked for it
    if !can_spawn_zebrad_for_test_type(test_name, test_type, use_internet_connection) {
        return Ok(None);
    }

    // Get the zebrad config
    let config = test_type
        .zebrad_config(
            test_name,
            use_internet_connection,
            replace_cache_dir,
            &network,
        )
        .expect("already checked config")?;

    let (zebrad_failure_messages, zebrad_ignore_messages) = test_type.zebrad_failure_messages();

    // Writes a configuration that does not have RPC listen_addr set.
    // If the state path env var is set, uses it in the config.
    let zebrad = testdir
        .with_exact_config(&config)?
        .spawn_child(args!["start"])?
        .bypass_test_capture(true)
        .with_timeout(test_type.zebrad_timeout())
        .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

    Ok(Some(zebrad))
}

/// Returns `true` if a zebrad test for `test_type` has everything it needs to run.
#[tracing::instrument]
pub fn can_spawn_zebrad_for_test_type<S: AsRef<str> + Debug>(
    test_name: S,
    test_type: TestType,
    use_internet_connection: bool,
) -> bool {
    if use_internet_connection && zebra_test::net::zebra_skip_network_tests() {
        return false;
    }

    // Skip the test unless the user specifically asked for it
    //
    // TODO: pass test_type to zebra_skip_lightwalletd_tests() and check for lightwalletd launch in there
    if test_type.launches_lightwalletd() && zebra_skip_lightwalletd_tests() {
        return false;
    }

    // Check if we have any necessary cached states for the zebrad config.
    // The cache_dir and network values don't matter here.
    test_type
        .zebrad_config(test_name, true, None, &Mainnet)
        .is_some()
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
