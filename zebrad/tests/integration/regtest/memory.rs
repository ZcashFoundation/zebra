//! Regtest nodes under a memory budget, inside a cgroup memory limit when systemd's user manager
//! can create one (design §6.3).
//!
//! Without a user manager the nodes run without a memory limit, and the tests check only what
//! the node itself reports.
//!
//! Run locally: `cargo nextest run -p zebrad -E 'test(/^integration::regtest::memory/)'`.

use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::Command,
    time::Duration,
};

use color_eyre::eyre::{eyre, Result};
use tempfile::TempDir;

use zebra_chain::{
    block,
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_test::{
    args,
    command::{Arguments, TestChild, TestDirExt},
    prelude::*,
};
use zebrad::{
    components::resources::{watchdog::ProcessMemory, MemoryBudget},
    config::ZebradConfig,
};

use crate::common::{
    config::{default_test_config, rpc_port_config, testdir},
    launch::{wait_for_mempool_activation, ZebradTestDirExt},
    regtest::MiningRpcMethods,
};

/// One mebibyte, in bytes.
const MIB: u64 = 1024 * 1024;

/// The longest a test runs.
const TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// The memory a test's cgroup allows above the node's budget: the budget's slack covers the
/// allocator, and this covers the page cache the cgroup also counts.
const CGROUP_HEADROOM: u64 = 100 * MIB;

/// Logs only what the tests read, so that mining many blocks never fills the log pipe.
const TRACING_FILTER: &str = "warn,zebrad::components::resources=info,zebrad::commands::start=info";

/// Returns whether systemd's user manager can put a process in a memory-limited scope.
fn memory_scopes_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|output| {
            matches!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "running" | "degraded"
            )
        })
}

/// Spawns `zebrad` with `extra_args` and the config in `dir`, inside a systemd scope whose memory
/// is limited to `cap` bytes, without swap, when the user manager allows it.
///
/// Returns the child and whether it runs under the limit.
fn spawn_capped(
    dir: TempDir,
    cap: u64,
    extra_args: Arguments,
) -> Result<(TestChild<TempDir>, bool)> {
    if !memory_scopes_available() {
        tracing::warn!(
            "systemd's user manager is not running: zebrad runs without a cgroup memory limit, \
             and the cgroup checks are skipped"
        );
        return Ok((dir.spawn_child(extra_args)?, false));
    }

    let config_path = dir.path().join("zebrad.toml");
    let config_path = config_path
        .to_str()
        .ok_or_else(|| eyre!("the test directory is valid Unicode"))?
        .to_string();

    // `systemd-run --scope` runs the command in place, so the child is zebrad itself.
    let mut args = Arguments::new();
    for argument in [
        "--user".to_string(),
        "--scope".to_string(),
        "--quiet".to_string(),
        format!("--property=MemoryMax={cap}"),
        "--property=MemorySwapMax=0".to_string(),
        env!("CARGO_BIN_EXE_zebrad").to_string(),
    ] {
        args.set_argument(argument);
    }
    args.set_parameter("-c", config_path);
    args.merge_with(extra_args);

    Ok((dir.spawn_child_with_command("systemd-run", args)?, true))
}

/// Returns the cgroup v2 directory of the process `pid`.
fn cgroup_dir(pid: u32) -> Result<PathBuf> {
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| eyre!("process {pid} has no cgroup v2 path"))?;

    Ok(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// Returns the `key` counter of the `memory.events` file in `dir`.
fn memory_event(dir: &std::path::Path, key: &str) -> Result<u64> {
    let events = fs::read_to_string(dir.join("memory.events"))?;
    events
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
        .ok_or_else(|| eyre!("memory.events has no {key} counter"))?
        .parse()
        .map_err(|error| eyre!("malformed {key} counter: {error}"))
}

/// Returns the process `pid`'s memory.
fn process_memory(pid: u32) -> Result<ProcessMemory> {
    ProcessMemory::parse_status(&fs::read_to_string(format!("/proc/{pid}/status"))?)
        .ok_or_else(|| eyre!("process {pid} has no VmRSS"))
}

/// Returns the Prometheus metrics the node at `address` exports.
fn scrape_metrics(address: SocketAddr) -> Result<String> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(b"GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    Ok(response)
}

/// Returns an unused local port.
fn unused_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

/// Returns a Regtest config with RPC on `rpc_port`, metrics on `metrics_port` and `budget`.
fn budget_config(budget: MemoryBudget, rpc_port: u16, metrics_port: u16) -> Result<ZebradConfig> {
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );
    let mut config = rpc_port_config(rpc_port, false, &network)?;
    config.mempool.debug_enable_at_height = Some(0);
    config.metrics.endpoint_addr = Some(SocketAddr::from(([127, 0, 0, 1], metrics_port)));
    config.tracing.filter = Some(TRACING_FILTER.to_string());
    config.resources.memory_budget = Some(budget);

    Ok(config)
}

/// Starts a node with `budget_mib` inside a cgroup limited to the budget plus
/// [`CGROUP_HEADROOM`], mines `blocks`, and checks it stays within its memory and exports the
/// memory metrics (design §6.3 cases (a) and (d)).
///
/// `profile` is the profile the budget opens with; Regtest's only checkpoint is its genesis
/// block, so a node always switches to, or stays at, the tip-only profile once its state opens.
async fn mine_within_budget(budget_mib: u64, profile: &str, blocks: u32) -> Result<()> {
    let budget = budget_mib * MIB;
    let (rpc_port, metrics_port) = (unused_port()?, unused_port()?);
    let mut config = budget_config(MemoryBudget::Bytes(budget), rpc_port, metrics_port)?;
    let rpc_address = config.rpc.listen_addr.expect("the config enables RPC");

    let dir = testdir()?.with_config(&mut config)?;
    let (child, capped) = spawn_capped(dir, budget + CGROUP_HEADROOM, args!["start"])?;
    let mut child = child.with_timeout(TEST_TIMEOUT)?;
    let pid = child.id();

    // The budget is logged in its largest exact unit, like "1 GiB" or "1113 MiB".
    child.expect_stdout_line_matches(format!(
        "memory budget {} \\(explicit\\) profile={profile} ",
        MemoryBudget::Bytes(budget)
    ))?;
    if profile == "ibd" {
        child.expect_stdout_line_matches("memory profile changed: ibd → tip_only")?;
    }

    let client = RpcRequestClient::new_with_timeout(rpc_address, TEST_TIMEOUT);
    wait_for_mempool_activation(&client).await?;
    client.generate(blocks).await?;
    let info = client.blockchain_info().await?;
    assert_eq!(info.blocks().0, blocks, "every block was mined");

    let metrics = scrape_metrics(SocketAddr::from(([127, 0, 0, 1], metrics_port)))?;
    assert!(
        metrics.contains(&format!("zebra_memory_budget_bytes {budget}")),
        "the budget gauge is exported: {metrics}"
    );
    assert!(
        metrics.contains("zebra_memory_profile 0"),
        "the node runs the tip-only profile"
    );
    assert!(
        metrics.contains("zebra_process_rss_bytes "),
        "the process memory gauges are exported"
    );
    assert!(
        metrics.contains("zebra_memory_pressure 0"),
        "a node well within its budget reports normal pressure"
    );
    assert!(
        metrics.contains("sync_pipeline_charged_bytes 0"),
        "the syncer was constructed with its derived pipeline bound: {metrics}"
    );
    let non_finalized_usage = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let metrics = scrape_metrics(SocketAddr::from(([127, 0, 0, 1], metrics_port)))?;
            let usage = metrics
                .lines()
                .find_map(|line| {
                    line.strip_prefix("zebra_memory_usage_bytes{component=\"non_finalized\"} ")
                })
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(0.0);
            if usage > 0.0 {
                return Ok::<_, color_eyre::Report>(usage);
            }

            // The watchdog samples every ten seconds, so the startup sample can precede mining.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    assert!(
        non_finalized_usage > 0.0,
        "mined non-finalized blocks contribute their carried charges"
    );

    let memory = process_memory(pid)?;
    assert!(
        memory.vm_hwm_bytes < budget,
        "the node's peak resident memory {} stays below its {budget} byte budget",
        memory.vm_hwm_bytes
    );

    if capped {
        let cgroup = cgroup_dir(pid)?;
        assert_eq!(
            fs::read_to_string(cgroup.join("memory.max"))?.trim(),
            (budget + CGROUP_HEADROOM).to_string(),
            "the node runs under the test's memory limit"
        );
        assert_eq!(
            memory_event(&cgroup, "oom_kill")?,
            0,
            "nothing was OOM-killed"
        );
        if let Ok(peak) = fs::read_to_string(cgroup.join("memory.peak")) {
            let peak: u64 = peak.trim().parse()?;
            assert!(
                peak <= budget + CGROUP_HEADROOM,
                "the cgroup peak {peak} fits"
            );
        }
    } else {
        tracing::warn!("skipped the cgroup checks: no memory-limited scope");
    }

    child.kill(false)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

/// A node at the 1,113 MiB initial sync minimum mines a few hundred blocks inside a cgroup
/// limited to its budget, without being OOM-killed (design §6.3 case (a), at the `Ibd` tier).
#[tokio::test(flavor = "multi_thread")]
async fn ibd_minimum_budget_mines_blocks_within_its_cgroup() -> Result<()> {
    let _init_guard = zebra_test::init();

    mine_within_budget(1113, "ibd", 300).await
}

/// A node at a 1 GiB tip-only budget starts on Regtest, whose only checkpoint is genesis, so the
/// initial sync refusal does not apply, and mines blocks inside its cgroup (design §6.3 case
/// (a), D15).
#[tokio::test(flavor = "multi_thread")]
async fn tip_only_budget_starts_on_regtest() -> Result<()> {
    let _init_guard = zebra_test::init();

    mine_within_budget(1024, "tip_only", 20).await
}

/// A tip-only budget refuses a node whose state is below the last checkpoint, once the state is
/// open (D15). Testnet has checkpoints above genesis, and the node needs no peers to refuse.
#[test]
fn tip_only_budget_refuses_initial_sync() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut config = default_test_config(&Network::new_default_testnet());
    config.tracing.filter = Some(TRACING_FILTER.to_string());
    config.resources.memory_budget = Some(MemoryBudget::Bytes(1024 * MIB));
    let output = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .with_timeout(TEST_TIMEOUT)?
        .wait_with_output()?;

    let output = output.assert_failure()?;
    output.stdout_line_contains(
        "IBD needs >= 1,113 MiB: sync from a snapshot or raise the budget (memory_budget 1 GiB \
         is only enough for a node at the tip, and the state tip at height 0 is below the last \
         checkpoint",
    )?;

    Ok(())
}

/// A byte-bounded initial sync rejects a custom checkpoint gap above 400 blocks, but the same
/// checkpoint list remains valid without a memory budget.
#[test]
fn checkpoint_gap_guard_only_applies_to_budgeted_ibd() -> Result<()> {
    let _init_guard = zebra_test::init();
    let genesis_hash = Network::new_regtest(Default::default()).genesis_hash();
    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        },
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (block::Height(0), genesis_hash),
            (block::Height(401), block::Hash([1; 32])),
        ])),
        ..Default::default()
    });
    let mut config = budget_config(
        MemoryBudget::Bytes(2048 * MIB),
        unused_port()?,
        unused_port()?,
    )?;
    config.network.network = network;

    let output = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .with_timeout(Duration::from_secs(60))?
        .wait_with_output()?;
    output.assert_failure()?.stdout_line_contains(
        "memory budget cannot sync a checkpoint list with a gap of 401 blocks ending at height \
         401; the maximum checkpoint gap is 400 blocks",
    )?;

    config.resources.memory_budget = None;
    let mut child = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .with_timeout(Duration::from_secs(60))?;
    child.expect_stdout_line_matches("spawned initial Zebra tasks")?;
    child.kill(false)?;
    child.wait_with_output()?.assert_failure()?;

    Ok(())
}

/// A budget below the floors refuses to start, listing the shortfall (design §6.3 case (b),
/// D4).
#[test]
fn below_floor_budget_refuses_to_start() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut config = budget_config(
        MemoryBudget::Bytes(256 * MIB),
        unused_port()?,
        unused_port()?,
    )?;
    let output = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start"])?
        .with_timeout(TEST_TIMEOUT)?
        .wait_with_output()?;

    let output = output.assert_failure()?;
    output.stdout_line_contains(
        "memory_budget 256 MiB is below the 963 MiB minimum for a node at the tip (1,113 MiB for \
         initial sync): RocksDB needs 256 MiB",
    )?;

    Ok(())
}

/// Resource planning fails before a valid snapshot decision can dial its configured peer.
#[test]
fn budget_shortfall_precedes_snapshot_bootstrap() -> Result<()> {
    let _init_guard = zebra_test::init();
    let genesis_hash = Network::new_regtest(Default::default()).genesis_hash();
    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        },
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (block::Height(0), genesis_hash),
            (block::Height(2), block::Hash([2; 32])),
        ])),
        ..Default::default()
    });
    let commitment = StateSnapshotCommitment {
        height: block::Height(1),
        block_hash: block::Hash([1; 32]),
        manifest: ObjectHash([2; 32]),
    };
    let mut config = budget_config(
        MemoryBudget::Bytes(256 * MIB),
        unused_port()?,
        unused_port()?,
    )?;
    config.network.network = network.clone();
    config.state.ephemeral = false;
    config.state.prune_blocks_older_than = Some(10_000);
    config.sync.state_snapshot_sync = true;
    config.sync.state_snapshot = Some(commitment);
    config.sync.state_snapshot_peers = vec!["192.0.2.1:1".to_string()];

    let dir = testdir()?.with_config(&mut config)?;
    let store = zebra_state::state_snapshot::store::ArtifactStore::new(&config.state, &network);
    assert!(matches!(
        zebrad::components::state_snapshot::decision::should_bootstrap(&config, &store),
        zebrad::components::state_snapshot::decision::Decision::Bootstrap { .. }
    ));
    let output = dir
        .spawn_child(args!["start"])?
        .with_timeout(TEST_TIMEOUT)?
        .wait_with_output()?;

    let output = output.assert_failure()?;
    output.stdout_line_contains("memory_budget 256 MiB is below the")?;
    let stdout = String::from_utf8_lossy(&output.output.stdout);
    assert!(
        !stdout.contains("state snapshot stage: bootstrap started"),
        "snapshot networking started before the memory plan: {stdout}"
    );

    Ok(())
}

/// `--unsafe-low-specs` starts a node below the floors, with every component at its floor and
/// a warning (design §6.3 case (c), D4).
#[test]
fn unsafe_low_specs_starts_below_the_floors() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut config = budget_config(
        MemoryBudget::Bytes(256 * MIB),
        unused_port()?,
        unused_port()?,
    )?;
    let mut child = testdir()?
        .with_config(&mut config)?
        .spawn_child(args!["start", "--unsafe-low-specs"])?
        .with_timeout(TEST_TIMEOUT)?;

    child.expect_stdout_line_matches(
        "--unsafe-low-specs: memory_budget 256 MiB is below the 963 MiB minimum",
    )?;
    child.expect_stdout_line_matches(
        "memory budget 256 MiB \\(explicit\\) profile=tip_only fixed=384 MiB",
    )?;
    child.expect_stdout_line_matches("spawned initial Zebra tasks")?;

    child.kill(false)?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}
