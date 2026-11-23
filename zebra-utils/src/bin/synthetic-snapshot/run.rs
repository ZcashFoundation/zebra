//! Fresh production bootstrap trials; the stopped-database oracle owns the final verdict.

use std::{
    collections::HashSet,
    fs,
    io::{self, Write},
    path::Path,
    time::{Duration, Instant},
};

use tokio::runtime::Runtime;
use zebra_state::{
    mock::prepare_serving_state,
    state_snapshot::{format::Manifest, store::ArtifactStore},
    BoxError, Config, MAX_BLOCK_REORG_HEIGHT,
};

use super::{
    config::Prepared,
    node::SnapshotNode,
    report::{self, Failure, RunReport, RunTimings, TrialReport, TrialTimings},
    verify::compare_installed,
};

const START_TIMEOUT: Duration = Duration::from_secs(120);
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Compile-time opt-in for the negative regression's after-stop hook.
///
/// Ordinary builds leave `SYNTHETIC_SNAPSHOT_TEST_HOOK` unset, so this is `false` and the
/// guarded code below is not emitted. The ignored Linux failure test documents building with
/// it set; that build runs the hook through the bounded, interruptible path.
const TEST_AFTER_STOP_HOOK: bool = option_env!("SYNTHETIC_SNAPSHOT_TEST_HOOK").is_some();

/// The after-stop hook is test scaffolding, not node work, so keep its wait short.
const AFTER_STOP_HOOK_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) fn command(
    config: &Path,
    zebrad: &Path,
    output: &Path,
    emit_json: bool,
) -> Result<(), BoxError> {
    let total = Instant::now();
    let admission = Instant::now();
    // Admission precedes workspace creation and every child launch.
    let prepared = config
        .canonicalize()
        .map_err(BoxError::from)
        .and_then(|config| Prepared::load(&config));
    let mut report = RunReport {
        config_path: config,
        configuration: prepared.as_ref().ok().map(|p| &p.config),
        prepared_state: prepared.as_ref().ok().map(|p| p.source.cache_dir.as_path()),
        zebrad,
        output,
        passed: false,
        failures: Vec::new(),
        timings_seconds: RunTimings {
            admission: admission.elapsed().as_secs_f64(),
            ..Default::default()
        },
        snapshot_bytes: None,
        prepared_state_bytes: None,
        serving_state_bytes: None,
        trials: Vec::new(),
    };
    // Never publish into a pre-existing output directory, even on failure.
    let mut report_path = None;
    let executed = prepared
        .as_ref()
        .map_err(|error| Failure::new("admission", error))
        .and_then(|prepared| execute(prepared, zebrad, output, &mut report, &mut report_path));
    report.failures.extend(executed.err());
    report.passed = report.failures.is_empty()
        && report
            .configuration
            .is_some_and(|config| u32::try_from(report.trials.len()).ok() == Some(config.trials))
        && report.trials.iter().all(|trial| trial.passed);
    report.timings_seconds.total = total.elapsed().as_secs_f64();
    if let Some(path) = &report_path {
        let saved = (|| -> Result<(), BoxError> {
            let mut file = tempfile::NamedTempFile::new_in(
                path.parent().ok_or("report path must have a parent")?,
            )?;
            serde_json::to_writer_pretty(&mut file, &report)?;
            writeln!(file)?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(())
        })();
        if let Err(error) = saved {
            report.passed = false;
            report.failures.push(Failure::new("report_output", error));
        }
    }
    if emit_json {
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, &report)?;
        writeln!(stdout)?;
        stdout.flush()?;
    }
    let passed = report.trials.iter().filter(|trial| trial.passed).count();
    let requested = report.configuration.map(|config| config.trials);
    eprintln!(
        "snapshot trials {}: {passed}/{} passed",
        if report.passed { "PASS" } else { "FAIL" },
        requested.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
    );
    for failure in &report.failures {
        eprintln!("{}: {}", failure.stage, failure.cause);
    }
    for trial in &report.trials {
        for failure in &trial.failures {
            eprintln!("trial {} {}: {}", trial.trial, failure.stage, failure.cause);
        }
    }
    if report.passed {
        Ok(())
    } else {
        Err("snapshot trial run failed; see JSON report and node logs".into())
    }
}

fn execute(
    prepared: &Prepared,
    zebrad: &Path,
    output: &Path,
    report: &mut RunReport<'_>,
    report_path: &mut Option<std::path::PathBuf>,
) -> Result<(), Failure> {
    let started = Instant::now();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| Failure::new("signal_setup", error))?;
    // Register before any child exists, including while constructing the serving view.
    let mut interruption = super::interrupt::Interruption::new(&runtime)
        .map_err(|error| Failure::new("signal_setup", error))?;
    // A failed attempt is still an attempted stage: report its elapsed time, including the
    // canonicalization attempt that shares the stage and owns its failure.
    let absolute_output = match fs::create_dir(output) {
        Ok(()) => {
            *report_path = Some(output.join("report.json"));
            let canonicalized = output.canonicalize();
            report.timings_seconds.output_setup = Some(started.elapsed().as_secs_f64());
            canonicalized.map_err(|error| Failure::new("output_setup", error))?
        }
        Err(error) => {
            report.timings_seconds.output_setup = Some(started.elapsed().as_secs_f64());
            return Err(Failure::new("output_setup", error));
        }
    };
    let output = absolute_output.as_path();
    eprintln!("run workspace: {}", output.display());

    let started = Instant::now();
    let measured = (|| {
        report.prepared_state_bytes = report::state_bytes(&prepared.source, &prepared.network)?;
        report.snapshot_bytes = Some({
            let commitment = prepared
                .config
                .resolved
                .as_ref()
                .expect("admission requires a resolved commitment")
                .commitment;
            let store = ArtifactStore::new(&prepared.source, &prepared.network);
            let bytes = fs::read(store.object_path(commitment.manifest))?;
            let manifest = Manifest::decode(&bytes, &prepared.network)?;
            let mut total = u64::try_from(bytes.len())?;
            let mut seen = HashSet::from([commitment.manifest]);
            for (_, _, piece) in manifest.pieces() {
                if seen.insert(piece.hash) {
                    total = total
                        .checked_add(fs::metadata(store.object_path(piece.hash))?.len())
                        .ok_or("snapshot size overflow")?;
                }
            }
            total
        });
        Ok::<(), BoxError>(())
    })();
    report.timings_seconds.prepared_sizes = Some(started.elapsed().as_secs_f64());
    measured.map_err(|error| Failure::new("prepared_sizes", error))?;

    let started = Instant::now();
    let workspace = tempfile::Builder::new()
        .prefix(".serving-")
        .tempdir_in(output);
    report.timings_seconds.source_fixture = Some(started.elapsed().as_secs_f64());
    let workspace = workspace.map_err(|error| Failure::new("source_fixture", error))?;
    let serving = Config {
        cache_dir: workspace.path().join("cache"),
        prune_blocks_older_than: Some(
            zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS.max(MAX_BLOCK_REORG_HEIGHT + 1),
        ),
        ..Default::default()
    };
    // Every path after creating the disposable view reaches measurement and cleanup below.
    let result = (|| {
        let resolved = prepared
            .config
            .resolved
            .as_ref()
            .expect("admission requires resolved configuration");
        let height = prepare_serving_state(
            &prepared.source,
            &prepared.network,
            prepared.config.tail,
            &serving,
            resolved.commitment,
            zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS + MAX_BLOCK_REORG_HEIGHT,
        );
        report.timings_seconds.source_fixture = Some(started.elapsed().as_secs_f64());
        let height = height.map_err(|error| Failure::new("source_fixture", error))?;
        let started = Instant::now();
        let source = runtime.block_on(interruption.until(SnapshotNode::start(
            zebrad,
            &prepared.network,
            &serving,
            &output.join("source"),
            height.0,
            prepared.config.trials,
            START_TIMEOUT,
        )));
        report.timings_seconds.source_startup = Some(started.elapsed().as_secs_f64());
        let mut source = source.map_err(|error| Failure::new("source_startup", error))?;
        eprintln!(
            "source A pid {} ready; commitment {}",
            source.id(),
            resolved.commitment.manifest
        );
        for number in 1..=prepared.config.trials {
            if let Err(error) = interruption.check() {
                report.failures.push(Failure::new("interruption", error));
                break;
            }
            let total = Instant::now();
            let trial_workspace = output.join(format!("trial-{number}"));
            let mut trial = TrialReport {
                trial: number,
                state: trial_workspace.join("cache"),
                passed: false,
                failures: Vec::new(),
                timings_seconds: TrialTimings::default(),
                state_bytes: None,
                comparison: None,
            };
            let result = run_trial(
                prepared,
                zebrad,
                &trial_workspace,
                &runtime,
                &mut source,
                &mut trial,
                &mut interruption,
            );
            trial.failures.extend(result.err());
            // No B handle remains here; measuring files never opens its database.
            let started = Instant::now();
            let state = Config {
                cache_dir: trial.state.clone(),
                ..Default::default()
            };
            match report::state_bytes(&state, &prepared.network) {
                Ok(bytes) => trial.state_bytes = bytes,
                Err(error) => trial.failures.push(Failure::new("state_size", error)),
            }
            trial.timings_seconds.state_size = Some(started.elapsed().as_secs_f64());
            trial.timings_seconds.total = total.elapsed().as_secs_f64();
            trial.passed = trial.failures.is_empty() && trial.comparison.is_some();
            let passed = trial.passed;
            eprintln!(
                "trial {number} {}: stopped-state comparison {}",
                if passed { "PASS" } else { "FAIL" },
                if trial.comparison.is_some() {
                    "completed"
                } else {
                    "not completed"
                }
            );
            report.trials.push(trial);
            if !passed {
                break;
            }
        }
        let started = Instant::now();
        let stopped = source.stop();
        report.timings_seconds.source_shutdown = Some(started.elapsed().as_secs_f64());
        stopped.map_err(|error| Failure::new("source_shutdown", error))
    })();
    report.failures.extend(result.err());
    let started = Instant::now();
    match report::state_bytes(&serving, &prepared.network) {
        Ok(bytes) => report.serving_state_bytes = bytes,
        Err(error) => report.failures.push(Failure::new("serving_size", error)),
    }
    report.timings_seconds.serving_size = Some(started.elapsed().as_secs_f64());
    let started = Instant::now();
    let cleanup = workspace.close();
    report.timings_seconds.cleanup = Some(started.elapsed().as_secs_f64());
    cleanup
        .map_err(|error| Failure::new("cleanup", error))
        .and_then(|()| {
            interruption
                .check()
                .map_err(|error| Failure::new("interruption", error))
        })
}

fn run_trial(
    prepared: &Prepared,
    zebrad: &Path,
    workspace: &Path,
    runtime: &tokio::runtime::Runtime,
    source: &mut SnapshotNode,
    trial: &mut TrialReport,
    interruption: &mut super::interrupt::Interruption,
) -> Result<(), Failure> {
    let resolved = prepared
        .config
        .resolved
        .as_ref()
        .expect("admission requires resolved configuration");
    let started = Instant::now();
    let imported = (|| {
        fs::create_dir(workspace)?;
        runtime.block_on(interruption.until(async {
            let start = SnapshotNode::start_bootstrap(
                zebrad,
                &prepared.network,
                workspace,
                resolved.commitment,
                source.quic_addr,
                START_TIMEOUT,
            );
            tokio::pin!(start);
            loop {
                tokio::select! {
                    result = &mut start => break result,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => source.require_running()?,
                }
            }
        }))
    })();
    trial.timings_seconds.bootstrap = Some(started.elapsed().as_secs_f64());
    let mut imported = imported.map_err(|error| Failure::new("bootstrap", error))?;
    eprintln!(
        "trial {} B pid {} installed; checking liveness",
        trial.trial,
        imported.id()
    );
    let started = Instant::now();
    let liveness = runtime.block_on(interruption.until(imported.check_liveness(
        source,
        resolved.expected_tip,
        resolved.commitment.height,
        LIVENESS_TIMEOUT,
    )));
    trial.timings_seconds.liveness = Some(started.elapsed().as_secs_f64());
    trial
        .failures
        .extend(liveness.err().map(|error| Failure::new("liveness", error)));
    // Always stop/reap, even after failed liveness; never compare after a shutdown error.
    let started = Instant::now();
    let stopped = imported.stop();
    trial.timings_seconds.shutdown = Some(started.elapsed().as_secs_f64());
    trial
        .failures
        .extend(stopped.err().map(|error| Failure::new("shutdown", error)));
    if !trial.failures.is_empty() {
        return Ok(());
    }
    interruption
        .check()
        .map_err(|error| Failure::new("interruption", error))?;
    // The ignored failure regression reaches this handoff: the trial node has been reaped and
    // its installed cache is closed, but the comparison has not read it yet. The hook is
    // compiled out unless the test build opts in, and its wait stays within the run's
    // cancellation and a deadline.
    if TEST_AFTER_STOP_HOOK {
        if let Some(hook) = std::env::var_os("TRIAL_TEST_AFTER_STOP") {
            run_after_stop_hook(runtime, &hook, &trial.state, interruption)?;
        }
    }
    let started = Instant::now();
    let comparison = compare_installed(prepared, &trial.state);
    trial.timings_seconds.comparison = Some(started.elapsed().as_secs_f64());
    trial.comparison = Some(comparison.map_err(|error| Failure::new("comparison", error))?);
    source
        .require_running()
        .map_err(|error| Failure::new("source_liveness", error))?;
    let started = Instant::now();
    let saved = (|| {
        serde_json::to_writer_pretty(
            fs::File::create(workspace.join("comparison.json"))?,
            &trial.comparison,
        )?;
        Ok::<(), BoxError>(())
    })();
    trial.timings_seconds.comparison_output = Some(started.elapsed().as_secs_f64());
    saved.map_err(|error| Failure::new("comparison_output", error))
}

/// Runs the test-only after-stop hook under the run's cancellation and a deadline.
///
/// The stopped database is mutated only after its node was reaped. The hook leads its own
/// process group, so cancellation or timeout stops the hook and everything it started before
/// the ordinary cleanup and report path continues; the immediate child is reaped here.
fn run_after_stop_hook(
    runtime: &Runtime,
    hook: &std::ffi::OsStr,
    state: &Path,
    interruption: &mut super::interrupt::Interruption,
) -> Result<(), Failure> {
    let mut command = tokio::process::Command::new(hook);
    command
        .arg(state)
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    // Registering the pidfd with the driver needs the owning runtime's context; the rest of
    // this helper runs outside an async block so cancellation still reaches the cleanup.
    let mut child = {
        let _entered = runtime.enter();
        command
            .spawn()
            .map_err(|error| Failure::new("comparison", error))?
    };
    let pid = child.id().expect("a spawned hook has a pid");
    let outcome = runtime.block_on(interruption.until(async {
        let status = tokio::time::timeout(AFTER_STOP_HOOK_TIMEOUT, child.wait())
            .await
            .map_err(|_| "TRIAL_TEST_AFTER_STOP timed out")?
            .map_err(BoxError::from)?;
        if status.success() {
            Ok(())
        } else {
            Err("TRIAL_TEST_AFTER_STOP failed".into())
        }
    }));
    // Always stop the hook's process group: even a hook that already exited can leave a
    // backgrounded child behind, and that child must not outlive the report.
    //
    // A negative pid targets the group the hook leads via `process_group(0)`, so this also
    // stops grandchildren. The POSIX shell's `kill` builtin sends the signal directly, so
    // cleanup neither depends on the caller's `PATH` nor on an external `kill` binary. An
    // already-gone group is successful cleanup: its absence is not an error.
    #[cfg(unix)]
    let _ = std::process::Command::new("/bin/sh")
        .args(["-c", &format!("kill -9 -{pid} 2>/dev/null || true")])
        .stderr(std::process::Stdio::null())
        .status();
    let _ = child.start_kill();
    let _ = runtime.block_on(child.wait());
    outcome.map_err(|error| Failure::new("comparison", error))
}
