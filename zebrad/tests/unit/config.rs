use std::{
    collections::HashSet, env, fs, io::Write as _, path::PathBuf, sync::Mutex, time::Duration,
};

use color_eyre::eyre::{eyre, Result, WrapErr};
use tempfile::{Builder, TempDir};

use zebra_chain::parameters::Network::*;
#[cfg(not(target_os = "windows"))]
use zebra_state;
use zebra_test::{args, command::to_regex::CollectRegexSet, prelude::*};
use zebrad::config::ZebradConfig;

use crate::common::{
    check::{EphemeralCheck, EphemeralConfig},
    config::{
        config_file_full_path, configs_dir, default_test_config, persistent_test_config, testdir,
    },
    launch::{ZebradTestDirExt, EXTENDED_LAUNCH_DELAY, LAUNCH_DELAY},
};

// Used by `non_blocking_logger` test, which is disabled on macOS.
#[cfg(not(target_os = "macos"))]
use crate::common::{
    config::{os_assigned_rpc_port_config, read_listen_addr_from_logs},
    sync::TINY_CHECKPOINT_TIMEOUT,
};
#[cfg(not(target_os = "macos"))]
use zebra_node_services::rpc_client::RpcRequestClient;
#[cfg(not(target_os = "macos"))]
use zebra_rpc::server::OPENED_RPC_ENDPOINT_MSG;

/// Check that the block state and peer list caches are written to disk.
#[test]
fn persistent_mode() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;
    let testdir = &testdir;

    let mut child = testdir.spawn_child(args!["-v", "start"])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(EXTENDED_LAUNCH_DELAY);
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let cache_dir = testdir.path().join("state");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "state directory empty despite persistent state config"
    );

    let cache_dir = testdir.path().join("network");
    assert_with_context!(
        cache_dir.read_dir()?.count() > 0,
        &output,
        "network directory empty despite persistent network config"
    );

    Ok(())
}

#[test]
fn ephemeral_existing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::ExistingDirectory)
}

#[test]
fn ephemeral_missing_directory() -> Result<()> {
    ephemeral(EphemeralConfig::Default, EphemeralCheck::MissingDirectory)
}

#[test]
fn misconfigured_ephemeral_existing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::ExistingDirectory,
    )
}

#[test]
fn misconfigured_ephemeral_missing_directory() -> Result<()> {
    ephemeral(
        EphemeralConfig::MisconfiguredCacheDir,
        EphemeralCheck::MissingDirectory,
    )
}

/// Check that the state directory created on disk matches the state config.
///
/// TODO: do a similar test for `network.cache_dir`
#[tracing::instrument]
fn ephemeral(cache_dir_config: EphemeralConfig, cache_dir_check: EphemeralCheck) -> Result<()> {
    use std::io::ErrorKind;

    let _init_guard = zebra_test::init();

    let mut config = default_test_config(&Mainnet)?;
    let run_dir = testdir()?;

    let ignored_cache_dir = run_dir.path().join("state");
    if cache_dir_config == EphemeralConfig::MisconfiguredCacheDir {
        // Write a configuration that sets both the cache_dir and ephemeral options
        config.state.cache_dir.clone_from(&ignored_cache_dir);
    }
    if cache_dir_check == EphemeralCheck::ExistingDirectory {
        // We set the cache_dir config to a newly created empty temp directory,
        // then make sure that it is empty after the test
        fs::create_dir(&ignored_cache_dir)?;
    }

    let mut child = run_dir
        .path()
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;
    // Run the program and kill it after a few seconds
    std::thread::sleep(EXTENDED_LAUNCH_DELAY);
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    let (expected_run_dir_file_names, optional_run_dir_file_names) = match cache_dir_check {
        // we created the state directory, so it should still exist
        EphemeralCheck::ExistingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect("ignored_cache_dir should still exist")
                    .count()
                    == 0,
                &output,
                "ignored_cache_dir not empty for ephemeral {:?} {:?}: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );
            (["state", "zebrad.toml"].iter(), ["network"].iter())
        }

        // we didn't create the state directory, so it should not exist
        EphemeralCheck::MissingDirectory => {
            assert_with_context!(
                ignored_cache_dir
                    .read_dir()
                    .expect_err("ignored_cache_dir should not exist")
                    .kind()
                    == ErrorKind::NotFound,
                &output,
                "unexpected creation of ignored_cache_dir for ephemeral {:?} {:?}: the cache dir exists and contains these files: {:?}",
                cache_dir_config,
                cache_dir_check,
                ignored_cache_dir.read_dir().unwrap().collect::<Vec<_>>()
            );

            (["zebrad.toml"].iter(), ["network"].iter())
        }
    };

    let expected_run_dir_file_names: HashSet<std::ffi::OsString> =
        expected_run_dir_file_names.map(Into::into).collect();

    let optional_run_dir_file_names: HashSet<std::ffi::OsString> =
        optional_run_dir_file_names.map(Into::into).collect();

    let run_dir_file_names = run_dir
        .path()
        .read_dir()
        .expect("run_dir should still exist")
        .map(|dir_entry| dir_entry.expect("run_dir is readable").file_name())
        // ignore directory list order, because it can vary based on the OS and filesystem
        .collect::<HashSet<_>>();

    let has_expected_file_paths = expected_run_dir_file_names
        .iter()
        .all(|expected_file_name| run_dir_file_names.contains(expected_file_name));

    let has_only_allowed_file_paths = run_dir_file_names.iter().all(|file_name| {
        optional_run_dir_file_names.contains(file_name)
            || expected_run_dir_file_names.contains(file_name)
    });

    assert_with_context!(
        has_expected_file_paths && has_only_allowed_file_paths,
        &output,
        "run_dir not empty for ephemeral {:?} {:?}: expected {:?}, actual: {:?}",
        cache_dir_config,
        cache_dir_check,
        expected_run_dir_file_names,
        run_dir_file_names
    );

    Ok(())
}

/// Run config tests that use the default ports and paths.
///
/// Unlike the other tests, these tests can not be run in parallel, because
/// they use the generated config. So parallel execution can cause port and
/// cache conflicts.
#[test]
fn config_tests() -> Result<()> {
    valid_generated_config("start", "Starting zebrad")?;

    // Check what happens when Zebra parses an invalid config
    invalid_generated_config()?;

    // Check that we have a current version of the config stored
    #[cfg(not(target_os = "windows"))]
    last_config_is_stored()?;

    // Check that Zebra's previous configurations still work
    stored_configs_work()?;

    // We run the `zebrad` app test after the config tests, to avoid potential port conflicts
    app_no_args()?;

    Ok(())
}

/// Test that `zebrad` runs the start command with no args
#[tracing::instrument]
fn app_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    // start caches state, so run one of the start tests with persistent state
    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;

    tracing::info!(?testdir, "running zebrad with no config (default settings)");

    let mut child = testdir.spawn_child(args![])?;

    // Run the program and kill it after a few seconds
    std::thread::sleep(LAUNCH_DELAY);
    child.kill(true)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_line_contains("Starting zebrad")?;

    // Make sure the command passed the legacy chain check
    output.stdout_line_contains("starting legacy chain check")?;
    output.stdout_line_contains("no legacy chain found")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

/// Test that `zebrad start` can parse the output from `zebrad generate`.
#[tracing::instrument]
fn valid_generated_config(command: &str, expect_stdout_line_contains: &str) -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    tracing::info!(?generated_config_path, "generating valid config");

    // Generate configuration in temp dir path
    let child =
        testdir.spawn_child(args!["generate", "-o": generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    tracing::info!(?generated_config_path, "testing valid config parsing");

    // Run command using temp dir and kill it after a few seconds
    let mut child = testdir.spawn_child(args![command])?;
    std::thread::sleep(LAUNCH_DELAY);
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_line_contains(expect_stdout_line_contains)?;

    // [Note on port conflict](#Note on port conflict)
    output.assert_was_killed().wrap_err("Possible port or cache conflict. Are there other acceptance test, zebrad, or zcashd processes running?")?;

    assert_with_context!(
        testdir.path().exists(),
        &output,
        "test temp directory not found"
    );
    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    Ok(())
}

/// Check if the config produced by current zebrad is stored.
#[cfg(not(target_os = "windows"))]
#[tracing::instrument]
#[allow(clippy::print_stdout)]
fn last_config_is_stored() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    tracing::info!(?generated_config_path, "generated current config");

    // Generate configuration in temp dir path
    let child =
        testdir.spawn_child(args!["generate", "-o": generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        generated_config_path.exists(),
        &output,
        "generated config file not found"
    );

    tracing::info!(
        ?generated_config_path,
        "testing current config is in stored configs"
    );

    // Get the contents of the generated config file
    let generated_content =
        fs::read_to_string(generated_config_path).expect("Should have been able to read the file");

    // We need to replace the cache dir path as stored configs has a dummy `cache_dir` string there.
    let processed_generated_content = generated_content
        .replace(
            zebra_state::Config::default()
                .cache_dir
                .to_str()
                .expect("a valid cache dir"),
            "cache_dir",
        )
        .trim()
        .to_string();

    // Loop all the stored configs
    //
    // TODO: use the same filename list code in last_config_is_stored() and stored_configs_work()
    for config_file in configs_dir()
        .read_dir()
        .expect("read_dir call failed")
        .flatten()
    {
        let config_file_path = config_file.path();
        let config_file_name = config_file_path
            .file_name()
            .expect("config files must have a file name")
            .to_string_lossy();

        if config_file_name.as_ref().starts_with('.') || config_file_name.as_ref().starts_with('#')
        {
            // Skip editor files and other invalid config paths
            tracing::info!(
                ?config_file_path,
                "skipping hidden/temporary config file path"
            );
            continue;
        }

        // Read stored config
        let stored_content = fs::read_to_string(config_file_full_path(config_file_path))
            .expect("Should have been able to read the file")
            .trim()
            .to_string();

        // If any stored config is equal to the generated then we are good.
        if stored_content.eq(&processed_generated_content) {
            return Ok(());
        }
    }

    println!(
        "\n\
         Here is the missing config file: \n\
         \n\
         {processed_generated_content}\n"
    );

    Err(eyre!(
        "latest zebrad config is not being tested for compatibility. \n\
         \n\
         Take the missing config file logged above, \n\
         and commit it to Zebra's git repository as:\n\
         zebrad/tests/common/configs/<next-release-tag>.toml \n\
         \n\
         Or run: \n\
         cargo build --bin zebrad && \n\
         zebrad generate | \n\
         sed 's/cache_dir = \".*\"/cache_dir = \"cache_dir\"/' > \n\
         zebrad/tests/common/configs/<next-release-tag>.toml",
    ))
}

/// Checks that Zebra prints an informative message when it cannot parse the
/// config file.
#[tracing::instrument]
fn invalid_generated_config() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = &testdir()?;

    // Add a config file name to tempdir path.
    let config_path = testdir.path().join("zebrad.toml");

    tracing::info!(
        ?config_path,
        "testing invalid config parsing: generating valid config"
    );

    // Generate a valid config file in the temp dir.
    let child = testdir.spawn_child(args!["generate", "-o": config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    assert_with_context!(
        config_path.exists(),
        &output,
        "generated config file not found"
    );

    // Load the valid config file that Zebra generated.
    let mut config_file = fs::read_to_string(config_path.to_str().unwrap()).unwrap();

    // Let's now alter the config file so that it contains a deprecated format
    // of `mempool.eviction_memory_time`.

    config_file = config_file
        .lines()
        // Remove the valid `eviction_memory_time` key/value pair from the
        // config.
        .filter(|line| !line.contains("eviction_memory_time"))
        .map(|line| line.to_owned() + "\n")
        .collect();

    // Append the `eviction_memory_time` key/value pair in a deprecated format.
    config_file += r"

            [mempool.eviction_memory_time]
            nanos = 0
            secs = 3600
    ";

    tracing::info!(?config_path, "writing invalid config");

    // Write the altered config file so that Zebra can pick it up.
    fs::write(config_path.to_str().unwrap(), config_file.as_bytes())
        .expect("Could not write the altered config file.");

    tracing::info!(?config_path, "testing invalid config parsing");

    // Run Zebra in a temp dir so that it loads the config.
    let mut child = testdir.spawn_child(args!["start"])?;

    // Return an error if Zebra is running for more than two seconds.
    //
    // Since the config is invalid, Zebra should terminate instantly after its
    // start. Two seconds should be sufficient for Zebra to read the config file
    // and terminate.
    std::thread::sleep(Duration::from_secs(2));
    if child.is_running() {
        // We're going to error anyway, so return an error that makes sense to the developer.
        child.kill(true)?;
        return Err(eyre!(
            "Zebra should have exited after reading the invalid config"
        ));
    }

    let output = child.wait_with_output()?;

    // Check that Zebra produced an informative message.
    output.stderr_contains(
        "Zebra could not load the provided configuration file and/or environment variables",
    )?;

    Ok(())
}

/// Test all versions of `zebrad.toml` we have stored can be parsed by the latest `zebrad`.
#[tracing::instrument]
#[test]
fn stored_configs_parsed_correctly() -> Result<()> {
    let old_configs_dir = configs_dir();
    use abscissa_core::Application;
    use zebrad::application::ZebradApp;

    tracing::info!(?old_configs_dir, "testing older config parsing");

    for config_file in old_configs_dir
        .read_dir()
        .expect("read_dir call failed")
        .flatten()
    {
        let config_file_path = config_file.path();
        let config_file_name = config_file_path
            .file_name()
            .expect("config files must have a file name")
            .to_str()
            .expect("config file names are valid unicode");

        if config_file_name.starts_with('.') || config_file_name.starts_with('#') {
            // Skip editor files and other invalid config paths
            tracing::info!(
                ?config_file_path,
                "skipping hidden/temporary config file path"
            );
            continue;
        }

        tracing::info!(
            ?config_file_path,
            "testing old config can be parsed by current zebrad"
        );

        ZebradApp::default()
            .load_config(&config_file_path)
            .expect("config should parse");
    }

    Ok(())
}

/// Test all versions of `zebrad.toml` we have stored can be parsed by the latest `zebrad`.
#[tracing::instrument]
fn stored_configs_work() -> Result<()> {
    let old_configs_dir = configs_dir();

    tracing::info!(?old_configs_dir, "testing older config parsing");

    for config_file in old_configs_dir
        .read_dir()
        .expect("read_dir call failed")
        .flatten()
    {
        let config_file_path = config_file.path();
        let config_file_name = config_file_path
            .file_name()
            .expect("config files must have a file name")
            .to_str()
            .expect("config file names are valid unicode");

        if config_file_name.starts_with('.') || config_file_name.starts_with('#') {
            // Skip editor files and other invalid config paths
            tracing::info!(
                ?config_file_path,
                "skipping hidden/temporary config file path"
            );
            continue;
        }

        let run_dir = testdir()?;
        let stored_config_path = config_file_full_path(config_file.path());

        tracing::info!(
            ?stored_config_path,
            "testing old config can be parsed by current zebrad"
        );

        // run zebra with stored config
        let mut child =
            run_dir.spawn_child(args!["-c", stored_config_path.to_str().unwrap(), "start"])?;

        let success_regexes = [
            // When logs are sent to the terminal, we see the config loading message and path.
            format!("Using config file at:.*{}", regex::escape(config_file_name)),
            // If they are sent to a file, we see a log file message on stdout,
            // and a logo, welcome message, and progress bar on stderr.
            "Sending logs to".to_string(),
            // TODO: add expect_stdout_or_stderr_line_matches() and check for this instead:
            //"Thank you for running a mainnet zebrad".to_string(),
        ];

        tracing::info!(
            ?stored_config_path,
            ?success_regexes,
            "waiting for zebrad to parse config and start logging"
        );

        let success_regexes = success_regexes
            .iter()
            .collect_regex_set()
            .expect("regexes are valid");

        // Zebra was able to start with the stored config.
        child.expect_stdout_line_matches(success_regexes)?;

        // finish
        child.kill(false)?;

        let output = child.wait_with_output()?;
        let output = output.assert_failure()?;

        // [Note on port conflict](#Note on port conflict)
        output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    }

    Ok(())
}

/// Test that Zebra's non-blocking logger works, by creating lots of debug output, but not reading the logs.
/// Then make sure Zebra drops excess log lines. (Previously, it would block waiting for logs to be read.)
///
/// This test is unreliable and sometimes hangs on macOS.
#[test]
#[cfg(not(target_os = "macos"))]
fn non_blocking_logger() -> Result<()> {
    use futures::FutureExt;
    use std::{sync::mpsc, time::Duration};

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (done_tx, done_rx) = mpsc::channel();

    let test_task_handle: tokio::task::JoinHandle<Result<()>> = rt.spawn(async move {
        let mut config = os_assigned_rpc_port_config(false, &Mainnet)?;
        config.tracing.filter = Some("trace".to_string());
        config.tracing.buffer_limit = 100;

        let dir = testdir()?.with_config(&mut config)?;
        let mut child = dir
            .spawn_child(args!["start"])?
            .with_timeout(TINY_CHECKPOINT_TIMEOUT);

        // Wait until port is open.
        let rpc_address = read_listen_addr_from_logs(&mut child, OPENED_RPC_ENDPOINT_MSG)?;

        // Create an http client
        let client = RpcRequestClient::new(rpc_address);

        // Most of Zebra's lines are 100-200 characters long, so 500 requests should print enough to fill the unix pipe,
        // fill the channel that tracing logs are queued onto, and drop logs rather than block execution.
        for _ in 0..500 {
            let res = client.call("getinfo", "[]".to_string()).await?;

            // Test that zebrad rpc endpoint is still responding to requests
            assert!(res.status().is_success());
        }

        child.kill(false)?;

        let output = child.wait_with_output()?;
        let output = output.assert_failure()?;

        // [Note on port conflict](#Note on port conflict)
        output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

        done_tx.send(())?;

        Ok(())
    });

    // Wait until the spawned task finishes up to 45 seconds before shutting down tokio runtime
    if done_rx.recv_timeout(Duration::from_secs(90)).is_ok() {
        rt.shutdown_timeout(Duration::from_secs(3));
    }

    match test_task_handle.now_or_never() {
        Some(Ok(result)) => result,
        Some(Err(error)) => Err(eyre!("join error: {:?}", error)),
        None => Err(eyre!("unexpected test task hang")),
    }
}

// ---------------------------------------------------------------------------
// Config loading tests (from config.rs)
// ---------------------------------------------------------------------------

const ZEBRA_ENV_PREFIX: &str = "ZEBRA_";

static TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Helper to isolate and manage ZEBRA_* environment variables in tests.
struct EnvGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    original_vars: Vec<(String, String)>,
}

impl EnvGuard {
    /// Acquire the global lock and clear all ZEBRA_* env vars, saving originals.
    fn new() -> Self {
        // If a test panics, the mutex guard is dropped, but the mutex remains poisoned.
        // We can recover from the poison error and get the lock, because we're going
        // to overwrite the environment variables anyway.
        let guard = TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let original_vars: Vec<(String, String)> = env::vars()
            .filter(|(key, _val)| key.starts_with(ZEBRA_ENV_PREFIX))
            .collect();

        for (key, _) in &original_vars {
            env::remove_var(key);
        }

        Self {
            _guard: guard,
            original_vars,
        }
    }

    /// Set a ZEBRA_* environment variable for this test.
    fn set_var(&self, key: &str, value: &str) {
        env::set_var(key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Clear any ZEBRA_* set during the test
        let current_vars: Vec<String> = env::vars()
            .filter(|(key, _)| key.starts_with(ZEBRA_ENV_PREFIX))
            .map(|(key, _)| key)
            .collect();
        for key in current_vars {
            env::remove_var(&key);
        }

        // Restore originals
        for (key, value) in &self.original_vars {
            env::set_var(key, value);
        }
    }
}

// --- Defaults and file loading ---

#[test]
fn config_load_defaults() {
    let _env = EnvGuard::new();

    let config = ZebradConfig::load(None).expect("Should load default config");

    assert_eq!(config.network.network.to_string(), "Mainnet");
    assert_eq!(config.rpc.listen_addr, None); // RPC disabled by default
    assert_eq!(config.metrics.endpoint_addr, None); // Metrics disabled by default
}

#[test]
fn config_load_from_file() {
    let _env = EnvGuard::new();

    let temp_dir = TempDir::new().expect("create temp dir");
    let config_path = temp_dir.path().join("test_config.toml");

    let test_config = r#"
[network]
network = "Testnet"

[rpc]
listen_addr = "127.0.0.1:8232"

[metrics]
endpoint_addr = "127.0.0.1:9999"
"#;

    fs::write(&config_path, test_config).expect("write test config");

    let config = ZebradConfig::load(Some(config_path)).expect("load config from file");

    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
    assert_eq!(
        config.metrics.endpoint_addr.unwrap().to_string(),
        "127.0.0.1:9999"
    );
}

#[test]
fn config_nonexistent_file_errors() {
    let _env = EnvGuard::new();

    let nonexistent_path = PathBuf::from("/this/path/does/not/exist.toml");
    ZebradConfig::load(Some(nonexistent_path)).expect_err("Should fail to load nonexistent file");
}

// --- Environment variable precedence ---

#[test]
fn config_env_override_defaults() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");
    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:8232");

    let config = ZebradConfig::load(None).expect("load config with env vars");

    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
}

#[test]
fn config_env_override_file() {
    let env = EnvGuard::new();

    let temp_dir = TempDir::new().expect("create temp dir");
    let config_path = temp_dir.path().join("test_config.toml");

    let test_config = r#"
[network]
network = "Mainnet"

[rpc]
listen_addr = "127.0.0.1:8233"
"#;

    fs::write(&config_path, test_config).expect("write test config");

    env.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");
    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:8232");

    let config = ZebradConfig::load(Some(config_path)).expect("load config");

    assert_eq!(config.network.network.to_string(), "Testnet");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:8232"
    );
}

#[test]
fn config_invalid_toml_errors() {
    let _env = EnvGuard::new();

    let temp_dir = TempDir::new().expect("create temp dir");
    let config_path = temp_dir.path().join("invalid_config.toml");

    let invalid_config = r#"
[network
network = "Testnet"
"#;

    fs::write(&config_path, invalid_config).expect("write invalid config");

    ZebradConfig::load(Some(config_path)).expect_err("Should fail to load invalid TOML");
}

#[test]
fn config_invalid_env_values_error() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "invalid_address");

    ZebradConfig::load(None).expect_err("Should fail with invalid RPC listen address");
}

#[test]
fn config_nested_env_vars() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_TRACING__FILTER", "debug");

    let config = ZebradConfig::load(None).expect("load config with nested env vars");

    assert_eq!(config.tracing.filter.as_deref(), Some("debug"));
}

// --- Specific env mappings used in Docker examples ---

#[test]
fn config_zebra_network_network_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_NETWORK__NETWORK", "Testnet");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_NETWORK__NETWORK");
    assert_eq!(config.network.network.to_string(), "Testnet");
}

#[test]
fn config_zebra_rpc_listen_addr_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_RPC__LISTEN_ADDR", "127.0.0.1:18232");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_RPC__LISTEN_ADDR");
    assert_eq!(
        config.rpc.listen_addr.unwrap().to_string(),
        "127.0.0.1:18232"
    );
}

#[test]
fn config_zebra_state_cache_dir_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_STATE__CACHE_DIR", "/test/cache");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_STATE__CACHE_DIR");
    assert_eq!(config.state.cache_dir, PathBuf::from("/test/cache"));
}

#[test]
fn config_zebra_metrics_endpoint_addr_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_METRICS__ENDPOINT_ADDR", "0.0.0.0:9999");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_METRICS__ENDPOINT_ADDR");
    assert_eq!(
        config.metrics.endpoint_addr.unwrap().to_string(),
        "0.0.0.0:9999"
    );
}

#[test]
fn config_zebra_tracing_log_file_env() {
    let env = EnvGuard::new();

    env.set_var("ZEBRA_TRACING__LOG_FILE", "/test/zebra.log");

    let config = ZebradConfig::load(None).expect("load config with ZEBRA_TRACING__LOG_FILE");
    assert_eq!(
        config.tracing.log_file.as_ref().unwrap(),
        &PathBuf::from("/test/zebra.log")
    );
}

#[test]
fn config_zebra_mining_miner_address_from_toml() {
    let _env = EnvGuard::new();

    let miner_address = "u1cymdny2u2vllkx7t5jnelp0kde0dgnwu0jzmggzguxvxj6fe7gpuqehywejndlrjwgk9snr6g69azs8jfet78s9zy60uepx6tltk7ee57jlax49dezkhkgvjy2puuue6dvaevt53nah7t2cc2k4p0h0jxmlu9sx58m2xdm5f9sy2n89jdf8llflvtml2ll43e334avu2fwytuna404a";
    let toml_string = format!(
        r#"[network]
        network = "Testnet"

        [mining]
        miner_address = "{miner_address}""#,
    );

    let mut file = Builder::new()
        .suffix(".toml")
        .tempfile()
        .expect("create temp file");
    file.write_all(toml_string.as_bytes())
        .expect("write temp file");

    let config = ZebradConfig::load(Some(file.path().to_path_buf()))
        .expect("load config with miner_address");

    assert_eq!(
        config.mining.miner_address.as_ref().unwrap().to_string(),
        miner_address
    );
}

// --- Sensitive env deny-list behaviour ---

#[test]
fn config_env_unknown_non_sensitive_key_errors() {
    let env = EnvGuard::new();

    // Unknown field without sensitive suffix should cause an error
    env.set_var("ZEBRA_MINING__FOO", "bar");

    ZebradConfig::load(None)
        .expect_err("Unknown non-sensitive env key should error (deny_unknown_fields)");
}

#[test]
fn config_env_unknown_sensitive_key_errors() {
    let env = EnvGuard::new();

    // Unknown field with sensitive suffix should cause an error
    env.set_var("ZEBRA_MINING__TOKEN", "secret-token");

    let result = ZebradConfig::load(None);
    assert!(result.is_err(), "Sensitive env key should cause an error");
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("sensitive key"), "error message: {}", msg);
}

#[test]
fn config_env_elasticsearch_password_errors() {
    let env = EnvGuard::new();

    // This key may or may not exist depending on features. It should be filtered regardless.
    env.set_var("ZEBRA_STATE__ELASTICSEARCH_PASSWORD", "topsecret");

    let result = ZebradConfig::load(None);
    assert!(result.is_err(), "Sensitive env key should cause an error");

    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("sensitive key"), "error message: {}", msg);
}
