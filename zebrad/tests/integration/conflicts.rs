//! Conflict Tests: Verifies Zebra's behavior when conflicting resources are used.

#![allow(clippy::unwrap_in_result)]

use std::{path::PathBuf, time::Duration};

use color_eyre::{
    eyre::{eyre, WrapErr},
    Help,
};

use zebra_chain::parameters::Network::Mainnet;
use zebra_state::constants::LOCK_FILE_ERROR;
use zebra_test::{args, command::ContextFrom, net::random_known_port, prelude::*};

#[cfg(not(target_os = "windows"))]
use zebra_network::constants::PORT_IN_USE_ERROR;

use crate::common::{
    config::{default_test_config, persistent_test_config, random_known_rpc_port_config, testdir},
    launch::{ZebradTestDirExt, BETWEEN_NODES_DELAY, LAUNCH_DELAY},
};

/// Test will start 2 zebrad nodes one after the other using the same Zcash listener.
/// It is expected that the first node spawned will get exclusive use of the port.
/// The second node will panic with the Zcash listener conflict hint added in #1535.
#[test]
#[cfg(not(target_os = "windows"))]
fn zebra_zcash_listener_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created network listen_addr
    let mut config = default_test_config(&Mainnet)?;
    config.network.listen_addr = listen_addr.parse().unwrap();
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!("Opened Zcash protocol endpoint at {listen_addr}"));

    // From another folder create a configuration with the same listener.
    // `network.listen_addr` will be the same in the 2 nodes.
    // (But since the config is ephemeral, they will have different state paths.)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same metrics listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic with the Zcash metrics
/// conflict hint added in #1535.
#[test]
#[cfg(all(feature = "prometheus", not(target_os = "windows")))]
fn zebra_metrics_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created metrics endpoint_addr
    let mut config = default_test_config(&Mainnet)?;
    config.metrics.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened metrics endpoint at {listen_addr}"));

    // From another folder create a configuration with the same endpoint.
    // `metrics.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same tracing listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic with the Zcash tracing
/// conflict hint added in #1535.
#[test]
#[cfg(all(feature = "filter-reload", not(target_os = "windows")))]
fn zebra_tracing_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created tracing endpoint_addr
    let mut config = default_test_config(&Mainnet)?;
    config.tracing.endpoint_addr = Some(listen_addr.parse().unwrap());
    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(r"Opened tracing endpoint at {listen_addr}"));

    // From another folder create a configuration with the same endpoint.
    // `tracing.endpoint_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, PORT_IN_USE_ERROR.as_str())?;

    Ok(())
}

/// Start 2 zebrad nodes using the same RPC listener port, but different
/// state directories and Zcash listener ports. The first node should get
/// exclusive use of the port. The second node will panic.
///
/// This test is sometimes unreliable on Windows, and hangs on macOS.
/// We believe this is a CI infrastructure issue, not a platform-specific issue.
#[test]
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn zebra_rpc_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    // Write a configuration that has RPC listen_addr set
    // [Note on port conflict](#Note on port conflict)
    //
    // This is the required setting to detect port conflicts.
    let mut config = random_known_rpc_port_config(false, &Mainnet)?;

    let dir1 = testdir()?.with_config(&mut config)?;
    let regex1 = regex::escape(&format!(
        r"Opened RPC endpoint at {}",
        config.rpc.listen_addr.unwrap(),
    ));

    // From another folder create a configuration with the same endpoint.
    // `rpc.listen_addr` will be the same in the 2 nodes.
    // But they will have different Zcash listeners (auto port) and states (ephemeral)
    let dir2 = testdir()?.with_config(&mut config)?;

    check_config_conflict(dir1, regex1.as_str(), dir2, "Address already in use")?;

    Ok(())
}

/// Start 2 zebrad nodes using the same state directory, but different Zcash
/// listener ports. The first node should get exclusive access to the database.
/// The second node will panic with the Zcash state conflict hint added in #1535.
#[test]
fn zebra_state_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // A persistent config has a fixed temp state directory, but asks the OS to
    // automatically choose an unused port
    let mut config = persistent_test_config(&Mainnet)?;
    let dir_conflict = testdir()?.with_config(&mut config)?;

    // Windows problems with this match will be worked on at #1654
    // We are matching the whole opened path only for unix by now.
    let contains = if cfg!(unix) {
        let mut dir_conflict_full = PathBuf::new();
        dir_conflict_full.push(dir_conflict.path());
        dir_conflict_full.push("state");
        dir_conflict_full.push(format!(
            "v{}",
            zebra_state::state_database_format_version_in_code().major,
        ));
        dir_conflict_full.push(config.network.network.to_string().to_lowercase());
        format!(
            "Opened Zebra state cache at {}",
            dir_conflict_full.display()
        )
    } else {
        String::from("Opened Zebra state cache at ")
    };

    check_config_conflict(
        dir_conflict.path(),
        regex::escape(&contains).as_str(),
        dir_conflict.path(),
        LOCK_FILE_ERROR.as_str(),
    )?;

    Ok(())
}

/// Launch a node in `first_dir`, wait a few seconds, then launch a node in
/// `second_dir`. Check that the first node's stdout contains
/// `first_stdout_regex`, and the second node's stderr contains
/// `second_stderr_regex`.
#[tracing::instrument]
fn check_config_conflict<T, U>(
    first_dir: T,
    first_stdout_regex: &str,
    second_dir: U,
    second_stderr_regex: &str,
) -> Result<()>
where
    T: ZebradTestDirExt + std::fmt::Debug,
    U: ZebradTestDirExt + std::fmt::Debug,
{
    // Start the first node
    let mut node1 = first_dir.spawn_child(args!["start"])?;

    // Wait until node1 has used the conflicting resource.
    node1.expect_stdout_line_matches(first_stdout_regex)?;

    // Wait a bit before launching the second node.
    std::thread::sleep(BETWEEN_NODES_DELAY);

    // Spawn the second node
    let node2 = second_dir.spawn_child(args!["start"]);
    let (node2, mut node1) = node1.kill_on_error(node2)?;

    // Wait a few seconds and kill first node.
    // Second node is terminated by panic, no need to kill.
    std::thread::sleep(LAUNCH_DELAY);
    let node1_kill_res = node1.kill(false);
    let (_, mut node2) = node2.kill_on_error(node1_kill_res)?;

    // node2 should have panicked due to a conflict. Kill it here anyway, so it
    // doesn't outlive the test on error.
    //
    // This code doesn't work on Windows or macOS. It's cleanup code that only
    // runs when node2 doesn't panic as expected. So it's ok to skip it.
    // See #1781.
    #[cfg(target_os = "linux")]
    if node2.is_running() {
        return node2
            .kill_on_error::<(), _>(Err(eyre!(
                "conflicted node2 was still running, but the test expected a panic"
            )))
            .context_from(&mut node1)
            .map(|_| ());
    }

    // Now we're sure both nodes are dead, and we have both their outputs
    let output1 = node1.wait_with_output().context_from(&mut node2)?;
    let output2 = node2.wait_with_output().context_from(&output1)?;

    // Make sure the first node was killed, rather than exiting with an error.
    output1
        .assert_was_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output2)?;

    // Make sure node2 has the expected resource conflict.
    output2
        .stderr_line_matches(second_stderr_regex)
        .context_from(&output1)?;
    output2
        .assert_was_not_killed()
        .warning("Possible port conflict. Are there other acceptance tests running?")
        .context_from(&output1)?;

    Ok(())
}

#[test]
#[cfg(not(target_os = "windows"))]
fn delete_old_databases() -> Result<()> {
    use std::fs::{canonicalize, create_dir};

    let _init_guard = zebra_test::init();

    // Skip this test because it can be very slow without a network.
    //
    // The delete databases task is launched last during startup, after network setup.
    // If there is no network, network setup can take a long time to timeout,
    // so the task takes a long time to launch, slowing down this test.
    if zebra_test::net::zebra_skip_network_tests() {
        return Ok(());
    }

    let mut config = default_test_config(&Mainnet)?;
    let run_dir = testdir()?;
    let cache_dir = run_dir.path().join("state");

    // create cache dir
    create_dir(cache_dir.clone())?;

    // create a v1 dir outside cache dir that should not be deleted
    let outside_dir = run_dir.path().join("v1");
    create_dir(&outside_dir)?;
    assert!(outside_dir.as_path().exists());

    // create a `v1` dir inside cache dir that should be deleted
    let inside_dir = cache_dir.join("v1");
    create_dir(&inside_dir)?;
    let canonicalized_inside_dir = canonicalize(inside_dir.clone()).ok().unwrap();
    assert!(inside_dir.as_path().exists());

    // modify config with our cache dir and not ephemeral configuration
    // (delete old databases function will not run when ephemeral = true)
    config.state.cache_dir = cache_dir;
    config.state.ephemeral = false;

    // run zebra with our config
    let mut child = run_dir
        .with_config(&mut config)?
        .spawn_child(args!["start"])?;

    // delete checker running
    child.expect_stdout_line_matches("checking for old database versions".to_string())?;

    // inside dir was deleted
    child.expect_stdout_line_matches(format!(
        "deleted outdated state database directory.*deleted_db.*=.*{canonicalized_inside_dir:?}"
    ))?;
    assert!(!inside_dir.as_path().exists());

    // deleting old databases task ended
    child.expect_stdout_line_matches("finished old database version cleanup task".to_string())?;

    // outside dir was not deleted
    assert!(outside_dir.as_path().exists());

    // finish
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

/// Check that the end of support code is called at least once.
#[test]
fn end_of_support_is_checked_at_start() -> Result<()> {
    let _init_guard = zebra_test::init();
    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet)?)?;
    let mut child = testdir.spawn_child(args!["start"])?;

    // Give enough time to start up the eos task.
    std::thread::sleep(Duration::from_secs(30));

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Zebra started
    output.stdout_line_contains("Starting zebrad")?;

    // End of support task started.
    output.stdout_line_contains("Starting end of support task")?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}
