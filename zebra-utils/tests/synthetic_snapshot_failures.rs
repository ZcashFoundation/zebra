//! CLI regressions for admission, child cleanup, and stopped-state comparison.
#![cfg(all(feature = "synthetic-snapshot", target_os = "linux"))]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
    time::Duration,
};

use zebra_state::{
    constants::STATE_DATABASE_KIND, state_database_format_version_in_code, BoxError, Config,
    DiskWriteBatch, ZebraDb,
};
use zebra_test::command::CommandExt;

fn invoke(command: &mut Command) -> Result<Output, BoxError> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn2((), "synthetic snapshot failure regression")
        .and_then(|child| child.with_timeout(Duration::from_secs(180)))
        .and_then(|child| child.wait_with_output())
        .map(|output| output.output)
        .map_err(|error| format!("CLI did not finish within its deadline: {error:?}").into())
}

/// Asserts `output` failed and returns the report it printed and published.
fn failed_report(output: &Output, destination: &Path) -> Result<serde_json::Value, BoxError> {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        report,
        serde_json::from_slice::<serde_json::Value>(&fs::read(destination.join("report.json"))?)?
    );
    Ok(report)
}

/// Asserts every process recorded by a wrapper has exited, and returns how many there were.
fn reaped_nodes(pids: &Path) -> Result<usize, BoxError> {
    let pids = fs::read_to_string(pids)?;
    for pid in pids.lines() {
        assert!(
            !Path::new("/proc").join(pid).exists(),
            "node {pid} was not reaped"
        );
    }
    Ok(pids.lines().count())
}

/// Waits for a hook child's recorded PID to disappear, then asserts it is gone.
fn reaped_hook_child(pid_file: &Path) -> Result<(), BoxError> {
    let pid = fs::read_to_string(pid_file)?;
    let pid = pid.trim();
    // Orphans reparent and are reaped asynchronously; allow a bounded settle window.
    for _ in 0..50 {
        if !Path::new("/proc").join(pid).exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("hook child {pid} outlived the report");
}

fn verify(config: &Path, state: &Path) -> Result<Output, BoxError> {
    invoke(
        Command::new(env!("CARGO_BIN_EXE_synthetic-snapshot"))
            .args(["verify", "--config"])
            .arg(config)
            .arg("--state")
            .arg(state),
    )
}

fn run(config: &Path, output: &Path, zebrad: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_synthetic-snapshot"));
    command
        .args(["run", "--config"])
        .arg(config)
        .arg("--output")
        .arg(output)
        .arg("--zebrad")
        .arg(zebrad);
    command
}

// Re-enter this test in the after-stop hook to remove one selected row from the stopped cache.
// Only trial 2's actual installed cache is changed; A and trial 1 stay untouched.
fn remove_prepared_anchor(cache_dir: &Path) -> Result<(), BoxError> {
    // The prepared regtest network activates NU5, which pruning validation requires.
    let network = zebra_chain::parameters::Network::new_regtest(
        zebra_chain::parameters::testnet::RegtestParameters {
            activation_heights: zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let source = ZebraDb::new(
        &Config {
            cache_dir: cache_dir.to_owned(),
            prune_blocks_older_than: Some(zebra_state::MAX_BLOCK_REORG_HEIGHT + 1),
            ..Default::default()
        },
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &network,
        true,
        std::iter::empty::<String>(),
        false,
    )?;
    let anchor = source.sapling_tree_for_tip().root();
    assert!(source.contains_sapling_anchor(&anchor));
    let mut batch = DiskWriteBatch::new();
    batch.delete_sapling_anchor(&source, &anchor);
    source.write_batch(batch)?;
    Ok(())
}

#[test]
fn prepare_preserves_existing_fixture_and_run_reports_a_damaged_database() -> Result<(), BoxError> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path();
    let config = root.join("trial.json");
    fs::write(
        &config,
        r#"{"state":"source","height":120,"tail":2,"trials":1}"#,
    )?;
    let prepare = || {
        invoke(
            Command::new(env!("CARGO_BIN_EXE_synthetic-snapshot"))
                .args(["prepare", "--config"])
                .arg(&config),
        )
    };
    let first = prepare()?;
    assert!(first.status.success(), "{first:?}");
    let prepared = fs::read(&config)?;

    let repeated = prepare()?;
    assert_eq!(repeated.status.code(), Some(1), "{repeated:?}");
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("already exists"));
    assert_eq!(fs::read(&config)?, prepared);

    let source = Config {
        cache_dir: root.join("source"),
        ..Default::default()
    };
    let db_path = source.db_path(
        STATE_DATABASE_KIND,
        state_database_format_version_in_code().major,
        &zebra_chain::parameters::Network::new_regtest(Default::default()),
    );
    // Remove RocksDB metadata but leave Zebra's format version intact.
    fs::remove_file(db_path.join("CURRENT"))?;
    let destination = root.join("results");
    let failed = invoke(&mut run(&config, &destination, &root.join("unused-zebrad")))?;
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let report: serde_json::Value = serde_json::from_slice(&failed.stdout)?;
    assert_eq!(report["failures"][0]["stage"], "admission");
    assert!(report["failures"][0]["cause"]
        .as_str()
        .is_some_and(|cause| cause.contains("cannot read prepared source")));
    assert!(!destination.exists());
    Ok(())
}

#[test]
#[ignore = "build ordinary zebrad and set ZEBRAD_BIN; prepares real RocksDB state"]
fn admission_and_interruption_report_failures_without_orphans() -> Result<(), BoxError> {
    if let Some(source) = std::env::var_os("TRIAL_TEST_REMOVE_ANCHOR") {
        return remove_prepared_anchor(Path::new(&source));
    }
    let zebrad = std::env::var_os("ZEBRAD_BIN").ok_or("set ZEBRAD_BIN")?;
    let zebrad = Path::new(&zebrad).canonicalize()?;
    let workspace = tempfile::tempdir()?;
    let root = workspace.path();
    let config = root.join("trial.json");
    fs::write(
        &config,
        r#"{"state":"source","height":120,"tail":2,"trials":3}"#,
    )?;
    let prepared = invoke(
        Command::new(env!("CARGO_BIN_EXE_synthetic-snapshot"))
            .args(["prepare", "--config"])
            .arg(&config),
    )?;
    assert!(prepared.status.success(), "{prepared:?}");

    let source = zebra_state::Config {
        cache_dir: root.join("source"),
        ..Default::default()
    };
    let version = source.version_file_path(
        zebra_state::constants::STATE_DATABASE_KIND,
        zebra_state::state_database_format_version_in_code().major,
        &zebra_chain::parameters::Network::new_regtest(Default::default()),
    );
    let original = fs::read(&version)?;
    fs::write(&version, "not-a-version")?;
    let destination = root.join("invalid-version");
    let failed = invoke(&mut run(&config, &destination, &zebrad))?;
    fs::write(&version, original)?;
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let report: serde_json::Value = serde_json::from_slice(&failed.stdout)?;
    assert_eq!(report["failures"][0]["stage"], "admission");
    assert!(report["failures"][0]["cause"]
        .as_str()
        .unwrap()
        .contains("version"));
    assert_eq!(report["trials"], serde_json::json!([]));
    assert!(!destination.exists());

    // Interrupt while source readiness is pending, then while B readiness is pending.
    // The wrapper is the owned node (exec preserves its PID), not an extra helper process.
    for (name, signal, pattern) in [
        ("source-interrupt", "TERM", "*/source/*"),
        ("bootstrap-interrupt", "INT", "*/trial-1/*"),
    ] {
        let wrapper = root.join(format!("{name}.sh"));
        let pids = root.join(format!("{name}.pids"));
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" >> \"$TRIAL_TEST_PIDS\"\ncase \"$2\" in\n  {pattern}) kill -{signal} \"$PPID\"; exec sleep 300;;\nesac\nexec \"$TRIAL_TEST_ZEBRAD\" \"$@\"\n"
            ),
        )?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        let destination = root.join(name);
        let failed = invoke(
            run(&config, &destination, &wrapper)
                .env("TRIAL_TEST_PIDS", &pids)
                .env("TRIAL_TEST_ZEBRAD", &zebrad),
        )?;
        let report = failed_report(&failed, &destination)?;
        assert_eq!(report["passed"], false);
        assert!(String::from_utf8_lossy(&failed.stdout).contains(&format!("SIG{signal}")));
        reaped_nodes(&pids)?;
        assert!(!fs::read_dir(&destination)?.any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".serving-")));
        if signal == "INT" {
            assert_eq!(report["trials"][0]["failures"][0]["stage"], "bootstrap");
            assert!(report["timings_seconds"]["source_shutdown"].is_number());
            assert_eq!(report["trials"].as_array().unwrap().len(), 1);
        } else {
            assert_eq!(report["failures"][0]["stage"], "source_startup");
            assert_eq!(report["trials"], serde_json::json!([]));
        }
    }

    // Trial 2 installs into its real cache, passes live checks, and is reaped. Only then does
    // the debug-only after-stop hook remove one selected row from that stopped cache, so A and
    // trial 1 stay unchanged and both `run` and standalone `verify` must reject trial 2.
    let destination = root.join("mismatch");
    let reaped = destination.join("trial-1/cache");
    let stopped = destination.join("trial-2/cache");
    let wrapper = root.join("mismatch.sh");
    let hook = root.join("after-stop.sh");
    let mutator = root.join("after-stop.pid");
    let pids = root.join("mismatch.pids");
    fs::write(
        &wrapper,
        "#!/bin/sh\nprintf '%s\\n' \"$$\" >> \"$TRIAL_TEST_PIDS\"\nexec \"$TRIAL_TEST_ZEBRAD\" \"$@\"\n",
    )?;
    // Mutate the stopped cache synchronously, then leave a backgrounded child behind: exiting
    // the hook must not orphan it.
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  */trial-2/*)\n    TRIAL_TEST_REMOVE_ANCHOR=\"$1\" '{}' --ignored --exact admission_and_interruption_report_failures_without_orphans\n    sleep 300 &\n    printf '%s\\n' \"$!\" > \"$TRIAL_TEST_HOOK_PID\"\n    ;;\nesac\n",
            std::env::current_exe()?.display()
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))?;
    let failed = invoke(
        run(&config, &destination, &wrapper)
            .env("TRIAL_TEST_PIDS", &pids)
            .env("TRIAL_TEST_ZEBRAD", &zebrad)
            .env("TRIAL_TEST_AFTER_STOP", &hook)
            .env("TRIAL_TEST_HOOK_PID", &mutator),
    )?;
    assert_eq!(reaped_nodes(&pids)?, 3, "{failed:?}");
    reaped_hook_child(&mutator)?;
    let report = failed_report(&failed, &destination)?;
    assert_eq!(report["passed"], false);
    assert_eq!(report["failures"], serde_json::json!([]));
    let trials = report["trials"].as_array().expect("trial reports");
    let [first, second] = trials.as_slice() else {
        panic!("expected one success and one failure: {report}");
    };
    assert_eq!(first["passed"], true);
    assert_eq!(first["comparison"]["height"], 120);
    assert_eq!(
        first["comparison"],
        serde_json::from_slice::<serde_json::Value>(&fs::read(
            destination.join("trial-1/comparison.json"),
        )?)?
    );
    assert_eq!(second["passed"], false);
    assert_eq!(second["comparison"], serde_json::Value::Null);
    // Reaching comparison requires live tip/prune/retention checks and shutdown to pass.
    assert!(
        second["timings_seconds"]["bootstrap"].is_number(),
        "{report}"
    );
    assert!(
        second["timings_seconds"]["liveness"].is_number(),
        "{report}"
    );
    let failures = second["failures"].as_array().expect("trial failures");
    assert_eq!(failures.len(), 1, "{report}");
    assert_eq!(failures[0]["stage"], "comparison", "{report}");
    let cause = failures[0]["cause"]
        .as_str()
        .expect("comparison failure cause");
    assert!(cause.contains("sapling_anchors"), "{cause}");
    assert!(
        cause.contains("key mismatch") || cause.contains("missing imported row"),
        "{cause}"
    );
    assert!(!destination.join("trial-3").exists());

    // Only trial 2's stopped state was changed: trial 1 still verifies against A.
    let verified = verify(&config, &reaped)?;
    assert_eq!(verified.status.code(), Some(0), "{verified:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&verified.stdout)?,
        first["comparison"]
    );

    let verified = verify(&config, &stopped)?;
    assert_eq!(verified.status.code(), Some(1), "{verified:?}");
    assert!(
        String::from_utf8_lossy(&verified.stderr).contains(cause),
        "{verified:?}"
    );

    // Cancellation during the after-stop hook must stop the hook's whole process group before
    // the report is published: the mutation child it backgrounded cannot outlive `run`.
    let destination = root.join("after-stop-interrupt");
    let hook = root.join("after-stop-interrupt.sh");
    let mutator = root.join("after-stop-interrupt.pid");
    let pids = root.join("after-stop-interrupt.pids");
    fs::write(
        &hook,
        "#!/bin/sh\ncase \"$1\" in\n  */trial-2/*)\n    sleep 300 &\n    printf '%s\\n' \"$!\" > \"$TRIAL_TEST_HOOK_PID\"\n    kill -TERM \"$PPID\"\n    wait\n    ;;\nesac\n",
    )?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))?;
    let output = invoke(
        run(&config, &destination, &wrapper)
            .env("TRIAL_TEST_PIDS", &pids)
            .env("TRIAL_TEST_ZEBRAD", &zebrad)
            .env("TRIAL_TEST_AFTER_STOP", &hook)
            .env("TRIAL_TEST_HOOK_PID", &mutator),
    )?;
    let report = failed_report(&output, &destination)?;
    assert_eq!(report["passed"], false);
    let trials = report["trials"].as_array().expect("trial reports");
    assert_eq!(trials.len(), 2, "{report}");
    assert_eq!(trials[0]["passed"], true);
    let failures = trials[1]["failures"].as_array().expect("trial failures");
    assert_eq!(failures[0]["stage"], "comparison", "{report}");
    let cause = failures[0]["cause"].as_str().expect("interruption cause");
    assert!(cause.contains("SIGTERM"), "{cause}");
    assert!(!destination.join("trial-3").exists());
    reaped_hook_child(&mutator)?;
    reaped_nodes(&pids)?;
    Ok(())
}
