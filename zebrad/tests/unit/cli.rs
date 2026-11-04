#[cfg(unix)]
use std::time::Duration;

use color_eyre::eyre::Result;

use zebra_chain::parameters::Network::*;
use zebra_test::{args, prelude::*};

use crate::common::{
    check::is_zebrad_version,
    config::{default_test_config, external_address_test_config, persistent_test_config, testdir},
    launch::{ZebradTestDirExt, LAUNCH_DELAY},
};

/// A shutdown signal must cancel startup even when no seed can resolve to a peer.
#[cfg(unix)]
#[test]
fn shutdown_during_seed_resolution() -> Result<()> {
    let _init_guard = zebra_test::init();

    for signal in ["-INT", "-TERM"] {
        let mut config = default_test_config(&Mainnet);
        config.network.initial_mainnet_peers = ["unresolvable.invalid:8233".to_owned()].into();
        let mut child = testdir()?
            .with_config(&mut config)?
            .spawn_child(args!["start"])?
            .with_timeout(Duration::from_secs(60))?;

        child.expect_stdout_line_matches("empty peer list after DNS resolution")?;
        crate::common::zcashd_compat::launch::send_signal(child.id(), signal)?;

        child
            .with_timeout(Duration::from_secs(30))?
            .wait_with_output()?
            .assert_success()?;
    }

    Ok(())
}

/// A shutdown signal must stop a managed zcashd while Zebra still forwards its output.
#[cfg(target_os = "linux")]
#[test]
fn shutdown_signal_stops_managed_zcashd_before_runtime() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    struct FakeZcashd(tempfile::TempDir);

    impl Drop for FakeZcashd {
        fn drop(&mut self) {
            let pid = std::fs::read_to_string(self.0.path().join("zcashd.pid"))
                .ok()
                .and_then(|pid| pid.trim().parse().ok());
            if let Some(pid) = pid {
                let _ = crate::common::zcashd_compat::launch::send_signal(pid, "-KILL");
            }
        }
    }

    let _init_guard = zebra_test::init();

    for graceful in [true, false] {
        let dir = testdir()?;
        // Keep the pid file alive after TestChild drops Zebra and its own test directory.
        let fake_zcashd = FakeZcashd(testdir()?);
        let zcashd_path = fake_zcashd.0.path().join("zcashd");
        std::fs::write(
            &zcashd_path,
            "#!/bin/sh\n\
             pid_file=\"$(dirname \"$0\")/zcashd.pid\"\n\
             trap 'echo fake zcashd stopping; sleep 1; rm -f \"$pid_file\"; exit 0' TERM\n\
             echo $$ > \"$pid_file\"\n\
             echo fake zcashd ready\n\
             while :; do sleep 1; done\n",
        )?;
        std::fs::set_permissions(&zcashd_path, std::fs::Permissions::from_mode(0o755))?;

        let network = zebra_chain::parameters::Network::new_regtest(Default::default());
        let mut config = default_test_config(&network);
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_path = Some(zcashd_path);
        config.zcashd_compat.zcashd_datadir = Some(dir.path().join("zcashd-datadir"));
        config.zcashd_compat.startup_delay = Duration::ZERO;
        let mut child = dir
            .with_config(&mut config)?
            .spawn_child(args!["start", "--unsafe-low-specs"])?
            .with_timeout(Duration::from_secs(60))?;

        child.expect_stdout_line_matches("fake zcashd ready")?;
        if graceful {
            crate::common::zcashd_compat::launch::send_signal(child.id(), "-TERM")?;

            // Runtime shutdown closes the child's output pipe, so only a supervised stop logs this.
            child.expect_stdout_line_matches("fake zcashd stopping")?;
            child.wait_with_output()?.assert_success()?;
        } else {
            let pid: u32 = std::fs::read_to_string(fake_zcashd.0.path().join("zcashd.pid"))?
                .trim()
                .parse()?;
            // Drop both owners as an early return after readiness or signal failure would.
            drop(child);
            drop(fake_zcashd);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let stopped = loop {
                match std::fs::read(format!("/proc/{pid}/cmdline")) {
                    Ok(command) if command.is_empty() => break true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break true,
                    Err(error) => return Err(error.into()),
                    Ok(_) if std::time::Instant::now() >= deadline => break false,
                    Ok(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            };
            if !stopped {
                let _ = crate::common::zcashd_compat::launch::send_signal(pid, "-KILL");
            }
            assert!(stopped, "the fake zcashd must not survive an early return");
        }
    }

    Ok(())
}

#[test]
fn generate_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let child = testdir()?
        .with_config(&mut default_test_config(&Mainnet))?
        .spawn_child(args!["generate"])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // First line
    output.stdout_line_contains("# Default configuration for zebrad")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // unexpected free argument `argument`
    let child = testdir.spawn_child(args!["generate", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(args!["generate", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // missing argument to option `-o`
    let child = testdir.spawn_child(args!["generate", "-o"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // Add a config file name to tempdir path
    let generated_config_path = testdir.path().join("zebrad.toml");

    // Valid
    let child =
        testdir.spawn_child(args!["generate", "-o": generated_config_path.to_str().unwrap()])?;

    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

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

#[test]
fn help_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet))?;

    let child = testdir.spawn_child(args!["help"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The first line should have the version
    output.any_output_line(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "are valid zebrad semantic versions",
    )?;

    // Make sure we are in help by looking for the usage string
    output.stdout_line_contains("Usage:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?;
    let testdir = &testdir;

    // The subcommand "argument" wasn't recognized.
    let child = testdir.spawn_child(args!["help", "argument"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // option `-f` does not accept an argument
    let child = testdir.spawn_child(args!["help", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn start_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    // start caches state, so run one of the start tests with persistent state
    let testdir = testdir()?.with_config(&mut persistent_test_config(&Mainnet)?)?;

    let mut child = testdir
        .spawn_child(args!["-v", "start"])?
        .with_timeout(LAUNCH_DELAY)?;

    child.expect_stdout_line_matches("Starting zebrad")?;
    child.expect_stdout_line_matches("starting legacy chain check")?;
    child.expect_stdout_line_matches("no legacy chain found")?;
    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    Ok(())
}

#[test]
fn start_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet))?;
    let testdir = &testdir;

    let mut child = testdir
        .spawn_child(args!["start"])?
        .with_timeout(LAUNCH_DELAY)?;
    child.expect_stdout_line_matches("Starting zebrad")?;
    child.kill(false)?;
    let output = child.wait_with_output()?;

    // Make sure the command was killed
    output.assert_was_killed()?;

    output.assert_failure()?;

    // unrecognized option `-f`
    let child = testdir.spawn_child(args!["start", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet))?;

    let child = testdir.spawn_child(args!["--version"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The output should only contain the build version, which deployment tooling parses
    output.output_check(
        |stdout| stdout.trim() == format!("zebrad {}", zebrad::application::build_version()),
        &output.output.stdout,
        "stdout",
        "contain only the zebrad build version",
    )?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    let testdir = testdir()?.with_config(&mut default_test_config(&Mainnet))?;
    let testdir = &testdir;

    // unrecognized option `-f`
    let child = testdir.spawn_child(args!["tip-height", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    // unrecognized option `-f` is ignored
    let child = testdir.spawn_child(args!["--version", "-f"])?;
    let output = child.wait_with_output()?;
    let output = output.assert_success()?;

    // The output should only contain the version
    output.output_check(
        is_zebrad_version,
        &output.output.stdout,
        "stdout",
        "a valid zebrad semantic version",
    )?;

    Ok(())
}

#[test]
fn external_address() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Zebra logs its external address when it sends a version message on an outbound
    // connection. A local listener accepts the TCP connection, so the node doesn't need any
    // live network peers.
    let peer_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let mut config = external_address_test_config(&Mainnet)?;
    config.network.initial_mainnet_peers = [peer_listener.local_addr()?.to_string()].into();

    let testdir = testdir()?.with_config(&mut config)?;
    let mut child = testdir
        .spawn_child(args!["start"])?
        .with_timeout(LAUNCH_DELAY)?;

    child.expect_stdout_line_matches("Starting zebrad")?;
    child.expect_stdout_line_matches("using external address for Version messages")?;

    child.kill(false)?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    // Make sure the command was killed.
    output.assert_was_killed()?;

    Ok(())
}
