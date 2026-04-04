//! Sync performance regression test infrastructure.
//!
//! Measures initial sync throughput across different chain eras by seeding
//! state via `copy-state` and syncing ~50k-block windows from the network.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use color_eyre::eyre::{eyre, Result};

use zebra_chain::{block::Height, parameters::Network};

use zebrad::components::sync;

use zebra_test::{args, prelude::*};

use super::config::{persistent_test_config, testdir};
use super::launch::ZebradTestDirExt;
use super::sync::{MIN_HEIGHT_FOR_DEFAULT_LOOKAHEAD, STOP_AT_HEIGHT_REGEX};

/// Default number of blocks to sync per sample.
const DEFAULT_SAMPLE_SIZE: u32 = 50_000;

/// Timeout per sample sync from the network.
const SAMPLE_SYNC_TIMEOUT: Duration = Duration::from_secs(60 * 60); // 1 hour

/// A block range to sample for performance measurement.
pub struct SampleRange {
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub description: &'static str,
}

/// Mainnet sample ranges covering each network upgrade era.
///
/// 230k blocks total as of this writing.
pub const MAINNET_SAMPLES: &[SampleRange] = &[
    SampleRange {
        name: "Genesis",
        start: 0,
        end: 10_000,
        description: "V1-V3 transparent-only",
    },
    SampleRange {
        name: "Early",
        start: 40_000,
        end: 50_000,
        description: "V1-V3 transparent-only",
    },
    SampleRange {
        name: "Overwinter-Sapling",
        start: 395_000,
        end: 420_000,
        description: "Overwinter (347,500) -> Sapling (419,200)",
    },
    SampleRange {
        name: "Blossom",
        start: 630_000,
        end: 670_000,
        description: "Blossom activation (653,600)",
    },
    SampleRange {
        name: "Heartwood",
        start: 880_000,
        end: 940_000,
        description: "Heartwood activation (903,000)",
    },
    SampleRange {
        name: "NU5",
        start: 1_670_000,
        end: 1_700_000,
        description: "NU5 (1,687,104), V5 txs + Orchard",
    },
    SampleRange {
        name: "Spam-attack",
        start: 1_820_000,
        end: 1_840_000,
        description: "Dense transparent dust transactions",
    },
    SampleRange {
        name: "Post-NU6.1",
        start: 3_100_000,
        end: 3_125_000,
        description: "Post-NU6.1, current chain",
    },
];

/// Testnet sample ranges.
pub const TESTNET_SAMPLES: &[SampleRange] = &[
    SampleRange {
        name: "Genesis",
        start: 0,
        end: 50_000,
        description: "Early testnet chain",
    },
    SampleRange {
        name: "Sapling",
        start: 260_000,
        end: 310_000,
        description: "Sapling activation (280,000)",
    },
    SampleRange {
        name: "Canopy",
        start: 1_010_000,
        end: 1_060_000,
        description: "Canopy activation (1,028,500)",
    },
    SampleRange {
        name: "NU5",
        start: 1_820_000,
        end: 1_870_000,
        description: "NU5 (1,842,420), V5 txs",
    },
    SampleRange {
        name: "Post-NU6.1",
        start: 3_500_000,
        end: 3_550_000,
        description: "Post-NU6.1, recent testnet",
    },
];

/// Result of syncing a single sample range.
struct SampleResult {
    name: &'static str,
    start: u32,
    end: u32,
    description: &'static str,
    copy_duration: Option<Duration>,
    sync_duration: Duration,
    blocks_synced: u32,
    blocks_per_sec: f64,
}

/// Run the performance benchmark for a network with the given sample ranges.
///
/// Uses the default Zebra cache directory as the source state, or
/// `ZEBRAD_PERF_SOURCE_STATE_DIR` if set. Optionally writes a markdown
/// report to `ZEBRAD_PERF_REPORT_PATH`.
pub fn sync_performance_benchmark(network: &Network, samples: &[SampleRange]) -> Result<()> {
    let _init_guard = zebra_test::init();

    // Use ZEBRAD_PERF_SOURCE_STATE_DIR if set, otherwise fall back to the
    // default Zebra cache directory. The env var name avoids the ZEBRA_ prefix
    // which Zebra's config loader interprets as config overrides.
    let source_state_dir = env::var("ZEBRAD_PERF_SOURCE_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| zebra_chain::common::default_cache_dir());

    if !source_state_dir.exists() {
        return Err(eyre!(
            "Source state directory does not exist: {}\n\
             Set ZEBRAD_PERF_SOURCE_STATE_DIR or sync Zebra first.",
            source_state_dir.display()
        ));
    }

    let sample_size = env::var("ZEBRAD_PERF_SAMPLE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SAMPLE_SIZE);

    let (commit, branch) = collect_git_info();
    let hardware = collect_hardware_info();
    let network_name = format!("{network}");

    eprintln!("=== Sync Performance Benchmark ===");
    eprintln!("Branch: {branch}  Commit: {commit}");
    eprintln!("Network: {network_name}  Sample size: {sample_size} blocks");
    eprintln!();

    let mut results = Vec::new();

    for sample in samples {
        // Allow overriding sample size via env var
        // let end = sample.start + sample_size;

        eprintln!(
            "--- {} ({} -> {}) ---",
            sample.name, sample.start, sample.end
        );

        let result = run_sample(&source_state_dir, sample, network)?;

        eprintln!(
            "  {} blocks in {:.1}s = {:.1} blocks/sec\n",
            result.blocks_synced,
            result.sync_duration.as_secs_f64(),
            result.blocks_per_sec,
        );

        results.push(result);
    }

    let report = format_report(&network_name, &commit, &branch, &hardware, &results);

    eprintln!("{report}");

    // Optionally write to file
    if let Ok(path) = env::var("ZEBRAD_PERF_REPORT_PATH") {
        std::fs::write(&path, &report)?;
        eprintln!("Report written to {path}");
    }

    Ok(())
}

/// Run a single sample: seed state via copy-state, then sync and measure.
fn run_sample(
    source_state_dir: &Path,
    sample: &SampleRange,
    network: &Network,
) -> Result<SampleResult> {
    let tempdir = testdir()?;

    // Phase 1: Copy state to start height (skip for genesis).
    // copy-state writes to tempdir/state/v27/{network}/ via the target config.
    let copy_duration = if sample.start > 0 {
        Some(copy_state_to_height(
            source_state_dir,
            tempdir.path(),
            sample.start,
            network,
        )?)
    } else {
        None
    };

    if let Some(d) = &copy_duration {
        eprintln!("  Copy to height {}: {:.1}s", sample.start, d.as_secs_f64());
    }

    // Phase 2: Sync from network, measuring wall-clock time.
    //
    // We manage the zebrad process directly instead of using `sync_until`
    // because `sync_until` with `ShouldNotActivate` uses `wait_with_output`,
    // which does NOT respect the timeout set by `with_timeout`. By using
    // `expect_stdout_line_matches` the deadline is enforced.
    let mut config = persistent_test_config(network)?;
    config.state.debug_stop_at_height = Some(sample.end);
    config.consensus.checkpoint_sync = true;

    if Height(sample.end) > MIN_HEIGHT_FOR_DEFAULT_LOOKAHEAD {
        config.sync.checkpoint_verify_concurrency_limit =
            sync::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;
    }

    let tempdir = tempdir.replace_config(&mut config)?;

    let sync_start = Instant::now();
    let mut child = tempdir
        .spawn_child(args!["start"])?
        .with_timeout(SAMPLE_SYNC_TIMEOUT);

    child.expect_stdout_line_matches(STOP_AT_HEIGHT_REGEX)?;
    let sync_duration = sync_start.elapsed();

    child.kill(true)?;
    let _tempdir = child.dir.take().expect("dir was not already taken");
    child.wait_with_output()?;

    let blocks_synced = sample.end - sample.start;
    let blocks_per_sec = blocks_synced as f64 / sync_duration.as_secs_f64();

    Ok(SampleResult {
        name: sample.name,
        start: sample.start,
        end: sample.end,
        description: sample.description,
        copy_duration,
        sync_duration,
        blocks_synced,
        blocks_per_sec,
    })
}

/// Use `copy-state` to populate target state up to `height`.
///
/// `target_dir` is used as both the working directory for config files and
/// the target `cache_dir` — copy-state writes state into `target_dir/state/v27/{network}/`.
fn copy_state_to_height(
    source_state_dir: &Path,
    target_dir: &Path,
    height: u32,
    network: &Network,
) -> Result<Duration> {
    // Write source config (used as zebrad's main config for copy-state)
    let mut source_config = persistent_test_config(network)?;
    source_config.state.cache_dir = source_state_dir.to_owned();

    let source_config_path = target_dir.join("source-zebrad.toml");
    let source_toml = toml::to_string(&source_config)?;
    std::fs::write(&source_config_path, source_toml)?;

    // Write target config with cache_dir = target_dir so state ends up at
    // target_dir/state/v27/{network}/, matching what sync_until expects.
    let mut target_config = persistent_test_config(network)?;
    target_config.state.cache_dir = target_dir.to_owned();

    let target_config_path = target_dir.join("target.toml");
    let target_toml = toml::to_string(&target_config)?;
    std::fs::write(&target_config_path, target_toml)?;

    let start = Instant::now();

    // Spawn copy-state as subprocess
    let zebrad_bin =
        env::var("ZEBRAD_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_zebrad").to_string());

    // Clear ZEBRA_* env vars from the subprocess so Zebra's config loader
    // doesn't interpret our test env vars (like ZEBRAD_PERF_*) as config
    // field overrides.
    let clean_env: Vec<(String, String)> = env::vars()
        .filter(|(k, _)| !k.starts_with("ZEBRA"))
        .collect();
    let output = Command::new(&zebrad_bin)
        .env_clear()
        .envs(clean_env)
        .args([
            "-c",
            source_config_path.to_str().unwrap(),
            "copy-state",
            "--max-source-height",
            &height.to_string(),
            "--target-config-path",
            target_config_path.to_str().unwrap(),
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("copy-state failed at height {height}: {stderr}"));
    }

    Ok(start.elapsed())
}

/// Sync from the current state tip to `end_height` and return the wall clock duration.
///
/// Reuses the provided `tempdir` so that state seeded by copy-state is preserved.
/// Collect git branch and commit info.
fn collect_git_info() -> (String, String) {
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    (commit, branch)
}

/// Collect basic hardware info for the report.
fn collect_hardware_info() -> String {
    let mut info = String::new();

    // CPU model
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
        if let Some(line) = cpuinfo.lines().find(|l| l.starts_with("model name")) {
            if let Some(model) = line.split(':').nth(1) {
                info.push_str(&format!("CPU: {}\n", model.trim()));
            }
        }
        let cores = cpuinfo
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count();
        info.push_str(&format!("Cores: {cores}\n"));
    }

    // RAM
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        if let Some(line) = meminfo.lines().find(|l| l.starts_with("MemTotal")) {
            if let Some(mem) = line.split_whitespace().nth(1) {
                if let Ok(kb) = mem.parse::<u64>() {
                    info.push_str(&format!("RAM: {} GB\n", kb / 1_048_576));
                }
            }
        }
    }

    // Kernel
    if let Ok(output) = Command::new("uname").arg("-r").output() {
        if let Ok(kernel) = String::from_utf8(output.stdout) {
            info.push_str(&format!("Kernel: {}", kernel.trim()));
        }
    }

    if info.is_empty() {
        "Unknown platform".to_string()
    } else {
        info
    }
}

/// Format the benchmark results as a markdown report.
fn format_report(
    network: &str,
    commit: &str,
    branch: &str,
    hardware: &str,
    results: &[SampleResult],
) -> String {
    let mut report = String::new();

    report.push_str("# Zebra Sync Performance Report\n\n");
    report.push_str(&format!("**Date:** {}\n", chrono::Utc::now().to_rfc3339()));
    report.push_str(&format!("**Branch:** {branch}\n"));
    report.push_str(&format!("**Commit:** {commit}\n"));
    report.push_str(&format!("**Network:** {network}\n\n"));

    report.push_str("## Hardware\n\n");
    report.push_str("```\n");
    report.push_str(hardware);
    report.push_str("\n```\n\n");

    report.push_str("## Results\n\n");
    report.push_str("| Sample | Heights | Duration | Blocks/sec | Copy Time | Description |\n");
    report.push_str("|--------|---------|----------|------------|-----------|-------------|\n");

    for r in results {
        let copy_time = r
            .copy_duration
            .map(|d| format!("{:.0}s", d.as_secs_f64()))
            .unwrap_or_else(|| "N/A".to_string());

        report.push_str(&format!(
            "| {} | {} → {} | {:.0}s | {:.1} | {} | {} |\n",
            r.name,
            r.start,
            r.end,
            r.sync_duration.as_secs_f64(),
            r.blocks_per_sec,
            copy_time,
            r.description,
        ));
    }

    report.push_str("\n## Notes\n\n");
    report.push_str("- Sync uses public network peers (not isolated)\n");
    report.push_str(
        "- Duration measures wall clock from `zebrad start` to `stopping at configured height`\n",
    );
    report.push_str("- Copy time is wall clock for `copy-state --max-source-height`\n");

    report
}
