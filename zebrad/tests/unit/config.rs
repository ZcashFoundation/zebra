//! Ephemeral Tests: Verifies Zebra's ephemeral state behavior.

#![allow(clippy::unwrap_in_result)]

use crate::assert_with_context;

use color_eyre::eyre::{eyre, WrapErr};
use std::{fs, time::Duration};

use crate::common::{
    config::{config_file_full_path, configs_dir, persistent_test_config, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

use zebra_chain::parameters::Network::Mainnet;
use zebra_test::{args, command::to_regex::CollectRegexSet, prelude::*};

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
