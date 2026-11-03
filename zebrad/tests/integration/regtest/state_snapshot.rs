//! Bootstrap an empty pruned node from a state snapshot served over the version 2 protocol
//! (design, §10 "Regtest e2e").
//!
//! Node A mines a Regtest chain, stops at a finalized tip of `H`, exports a snapshot with
//! `zebrad export-state-snapshot`, and restarts serving it. Node B starts empty with the exported
//! commitment and checkpoints generated from A's chain, downloads the snapshot over loopback, syncs
//! the blocks above `H` from A, and is checked for the properties a snapshot-born node promises. A
//! third empty node, configured without checkpoints, is refused by the decision rule and syncs
//! from genesis instead.
//!
//! Everything the test waits for is a stage line from
//! [`zebrad::components::state_snapshot::progress::Stage`], a decision-rule reason from
//! [`zebrad::components::state_snapshot::decision::SkipReason`], or an existing log line, so the
//! test compiles against the scaffold contract but passes only once the bootstrap is integrated.
//!
//! The test runs two nodes through a multi-minute bootstrap, so it is ignored by default:
//!
//! ```console
//! cargo nextest run -p zebrad --run-ignored=all \
//!     -E 'test(=integration::regtest::state_snapshot::pruned_node_bootstraps_from_a_state_snapshot)'
//! ```

use std::{
    collections::HashMap,
    fs,
    net::{SocketAddr, TcpListener, UdpSocket},
    path::Path,
    time::Duration,
};

use color_eyre::eyre::{eyre, Result};
use serde_json::{json, Value};
use tempfile::TempDir;
use zebra_chain::{
    amount::Amount,
    block::{self, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    serialization::{ZcashDeserialize, ZcashSerialize},
    state_snapshot::StateSnapshotCommitment,
    transaction::{Transaction, TransparentSigningKey},
    transparent::OutPoint,
};
use zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS;
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::{
    client::{BlockTemplateResponse, SendRawTransactionResponse},
    proposal_block_from_template,
};
use zebra_state::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    state_snapshot::{format::Manifest, store::ArtifactStore},
};
use zebra_test::{args, command::TestChild, prelude::*};
use zebrad::{
    components::state_snapshot::{
        decision::{Precondition, SkipReason},
        progress::Stage,
    },
    config::ZebradConfig,
};

use crate::common::{
    config::{rpc_port_config, testdir},
    launch::{wait_for_mempool_activation, ZebradTestDirExt},
    regtest::MiningRpcMethods,
};

/// The snapshot height `H`.
///
/// Above `NODE_NETWORK_LIMITED_BLOCKS`, so the window starts above genesis and the heights below
/// it have no bodies on the snapshot-born node.
const H: u32 = 2_400;

/// The window start `L = max(1, H - 2303)` (design, §3).
const L: u32 = H + 1 - NODE_NETWORK_LIMITED_BLOCKS;

/// Node A's retention: at least `NODE_NETWORK_LIMITED_BLOCKS`, so it advertises limited service to
/// version 2 peers, and more than A's highest finalized tip in the test, so A never deletes a body.
/// The exporter refuses a database pruned below `L` (design, §7), and the third node syncs from
/// genesis with A as its only peer, which stops offering blocks at its first missing body.
/// Configured pruning alone withdraws `NODE_NETWORK`, so A still counts as pruned (design, §10).
const RETENTION_A: u32 = 4_000;

/// Node B's retention, larger than the chain this test mines, so its retained floor stays at the
/// window start rather than moving up as its own pruning runs.
const RETENTION_B: u32 = 4_000;

/// The height of node B's last checkpoint above `H`, as blocks above `H`.
const BLOCKS_ABOVE_H: u32 = 1_000;

/// The blocks node A mines above the last checkpoint, which B verifies semantically.
const SEMANTIC_TAIL: u32 = 100;

/// The gap between the Regtest checkpoints generated from A's chain above `H + 2`.
const CHECKPOINT_GAP: u32 = 250;

// The constants fit together as the design requires (§3, §7, §10).
const _: () = assert!(L == H - 2303, "L is the window start");
const _: () = assert!(
    RETENTION_A >= NODE_NETWORK_LIMITED_BLOCKS,
    "A advertises limited service"
);
const _: () = assert!(
    RETENTION_A > H,
    "A still holds the window when it exports at H"
);
// A's best tip ends one block above `H + BLOCKS_ABOVE_H + SEMANTIC_TAIL`, the block B mines, and
// its finalized tip is the non-finalized depth below that.
const _: () = assert!(
    RETENTION_A > H + BLOCKS_ABOVE_H + SEMANTIC_TAIL + 1 - MAX_BLOCK_REORG_HEIGHT,
    "A never prunes a body, so C can sync from genesis through it"
);
const _: () = assert!(
    RETENTION_B > H + BLOCKS_ABOVE_H + SEMANTIC_TAIL + 1,
    "B never prunes during the test, so its floor stays at L"
);
const _: () = assert!(
    // u32 to usize is a lossless widening on every target Zebra supports.
    CHECKPOINT_GAP as usize <= zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP,
    "checkpoint gaps fit the verifier's download window"
);

/// The amount the spend of the pre-window coinbase output pays to itself.
const SPEND_ZATOSHIS: i64 = 100_000_000;

/// The whole test, under nextest's 20-minute backstop for the default profile.
const TEST_TIMEOUT: Duration = Duration::from_secs(900);

/// One node reaching a height, or one node accepting a block from another.
const SYNC_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a mined block takes to reach the other node by gossip.
const RELAY_TIMEOUT: Duration = Duration::from_secs(60);

/// Blocks per `generate` call, so each RPC call stays well inside the client timeout.
const GENERATE_BATCH: u32 = 500;

/// The stage lines node B must log, in order. `TargetReached` needs a configured sync target,
/// which B does not have.
const EXPECTED_STAGES: [Stage; 13] = [
    Stage::BootstrapStarted,
    Stage::PeersReady,
    Stage::ManifestVerified,
    Stage::FirstPieceVerified,
    Stage::LastPieceVerified,
    Stage::SstsBuilt,
    Stage::SstsIngested,
    Stage::ChecksDone,
    Stage::StateRenamed,
    Stage::StateInitialized,
    Stage::FirstCommitAboveSnapshot,
    Stage::SuccessorCommitted,
    Stage::LastCheckpointReached,
];

/// Reserves a loopback TCP port by binding and releasing it, like the pruning tests do, so the
/// node's config can name the port before its logs are readable.
fn reserve_tcp_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Reserves a loopback UDP port for a version 2 (QUIC) endpoint.
fn reserve_udp_port() -> Result<u16> {
    Ok(UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn loopback(port: u16) -> SocketAddr {
    ([127, 0, 0, 1], port).into()
}

/// Regtest with NU5 from height 1, so every coinbase is V5 and supplies the legacy-chain
/// witness, and with transparent coinbase spends allowed. `checkpoints` are the heights and
/// hashes node B verifies against; `None` keeps the Regtest default of genesis only.
fn regtest(checkpoints: Option<Vec<(Height, block::Hash)>>) -> Network {
    Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        },
        should_allow_unshielded_coinbase_spends: Some(true),
        checkpoints: checkpoints.map(ConfiguredCheckpoints::HeightsAndHashes),
        ..Default::default()
    })
}

/// A persistent, pruned, mining node on `network` with reserved RPC and legacy ports.
///
/// The crawler interval is long so loopback nodes do not open connections beyond the ones the
/// test accounts for in `max_connections_per_ip`.
fn node_config(
    network: &Network,
    rpc_port: u16,
    p2p_addr: SocketAddr,
    retention: u32,
    miner: &TransparentSigningKey,
) -> Result<ZebradConfig> {
    let mut config = rpc_port_config(rpc_port, false, network)?;
    config.network.listen_addr = p2p_addr;
    config.network.max_connections_per_ip = 4;
    config.network.crawl_new_peer_interval = Duration::from_secs(3600);
    config.state.ephemeral = false;
    config.state.prune_blocks_older_than = Some(retention);
    // Persist each non-finalized update before publishing the tip, including across restart.
    config.state.debug_skip_non_finalized_state_backup_task = true;
    config.mempool.debug_enable_at_height = Some(0);
    config.mining.miner_address = Some(miner.address(network.kind()).into());
    Ok(config)
}

/// Returns `getblockcount`, or `None` while the node has no tip yet.
async fn block_count(client: &RpcRequestClient) -> Option<u32> {
    client
        .json_result_from_call::<u32>("getblockcount", "[]")
        .await
        .ok()
}

/// Polls `getblockcount` until `node` reaches `height`.
async fn wait_for_height(
    client: &RpcRequestClient,
    height: u32,
    timeout: Duration,
    node: &str,
) -> Result<()> {
    tokio::time::timeout(timeout, async {
        loop {
            if block_count(client)
                .await
                .is_some_and(|count| count >= height)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| eyre!("{node} did not reach height {height} within {timeout:?}"))
}

/// Mines with `generate` in batches until the best tip is `best_tip`.
///
/// Returns an RPC error as the inner result instead of failing, so a node configured to stop at
/// a height can exit during or right after the batch that finalizes it.
async fn mine_to(client: &RpcRequestClient, best_tip: u32) -> Result<()> {
    loop {
        let have = block_count(client)
            .await
            .ok_or_else(|| eyre!("the mining node has no tip"))?;
        if have >= best_tip {
            return Ok(());
        }
        client
            .generate((best_tip - have).min(GENERATE_BATCH))
            .await?;
    }
}

async fn block_hash(client: &RpcRequestClient, height: u32) -> Result<block::Hash> {
    Ok(client
        .json_result_from_call::<String>("getblockhash", format!("[{height}]"))
        .await
        .map_err(|err| eyre!(err))?
        .parse()?)
}

async fn blockchain_info(client: &RpcRequestClient) -> Result<Value> {
    client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|err| eyre!(err))
}

/// The `getblock` error for `height` at verbosity 0 and 1: the JSON-RPC error object.
async fn get_block_error(client: &RpcRequestClient, height: u32, verbosity: u8) -> Result<Value> {
    let response: Value = serde_json::from_str(
        &client
            .text_from_call("getblock", format!(r#"["{height}",{verbosity}]"#))
            .await?,
    )?;
    if response["error"].is_null() {
        return Err(eyre!(
            "getblock {height} verbosity {verbosity} succeeded, but the body is below the window"
        ));
    }
    Ok(response["error"].clone())
}

/// The commitment `zebrad export-state-snapshot` prints as TOML on success.
fn parse_commitment(stdout: &str) -> Result<StateSnapshotCommitment> {
    let toml: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| {
            ["height = ", "hash = ", "manifest = "]
                .iter()
                .any(|key| line.starts_with(key))
        })
        .collect();
    toml::from_str(&toml.join("\n"))
        .map_err(|error| eyre!("export output has no commitment: {error}\n{stdout}"))
}

/// The total size of the regular files under `dir`, in bytes.
fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total += metadata.len();
            }
        }
    }
    Ok(total)
}

/// The size of the distinct piece objects: identical manifest entries are downloaded and stored once.
fn unique_piece_bytes(manifest: &Manifest) -> u64 {
    manifest
        .pieces()
        .map(|(_, _, piece)| (piece.hash, u64::from(piece.size)))
        .collect::<HashMap<_, _>>()
        .values()
        .sum()
}

/// Scrapes a node's Prometheus endpoint.
async fn scrape_metrics(addr: SocketAddr) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    Ok(client
        .get(format!("http://{addr}"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

/// Returns a counter's value from a Prometheus scrape, or `None` if it has not been recorded.
/// `command` selects the single command label used by the version 2 connection counters.
fn metric(body: &str, name: &str, command: Option<&str>) -> Result<Option<u64>> {
    let name = name.replace('.', "_");
    let total_name = format!("{name}_total");
    for line in body.lines().filter(|line| !line.starts_with('#')) {
        let Some((sample, value)) = line.split_once(' ') else {
            continue;
        };
        let (sample_name, labels) = sample
            .split_once('{')
            .map_or((sample, ""), |(name, labels)| {
                (name, labels.trim_end_matches('}'))
            });
        if sample_name != name && sample_name != total_name {
            continue;
        }
        let expected_labels =
            command.map_or("".to_owned(), |command| format!("command=\"{command}\""));
        if labels != expected_labels {
            continue;
        }
        return Ok(Some(value.parse()?));
    }
    Ok(None)
}

/// Checks actual object response traffic against the unique manifest and piece bytes.
fn assert_snapshot_traffic(metrics_a: &str, metrics_b: &str, expected_bytes: u64) -> Result<()> {
    let required = |body, name, command| {
        metric(body, name, command)?
            .ok_or_else(|| eyre!("missing required Prometheus counter {name}"))
    };
    let transport_bytes = required(metrics_a, "zebra.net.in.objects.bytes", None)?;
    let requests_a = required(metrics_a, "zebra.net.in.requests", Some("get-object"))?;
    let responses_a = required(metrics_a, "zebra.net.out.responses", Some("get-object"))?;
    let requests_b = required(metrics_b, "zebra.net.out.requests", Some("Object"))?;
    let responses_b = required(metrics_b, "zebra.net.in.responses", Some("Object"))?;
    let busy = metric(metrics_a, "zebra.net.in.objects.busy", None)?.unwrap_or(0);
    let unreadable = metric(metrics_a, "zebra.net.in.objects.unreadable", None)?.unwrap_or(0);
    let cancelled =
        metric(metrics_b, "zebra.net.out.requests.canceled", Some("Object"))?.unwrap_or(0);
    let failed_or_refused = requests_a
        .checked_sub(responses_a)
        .ok_or_else(|| eyre!("A sent more object responses than it received requests"))?;

    assert_eq!(
        transport_bytes, expected_bytes,
        "actual response payload bytes"
    );
    assert_eq!(requests_a, requests_b, "A and B count the same requests");
    assert_eq!(responses_a, responses_b, "A and B count the same responses");
    assert_eq!(
        failed_or_refused, 0,
        "no failed or refused streams to retry"
    );
    assert_eq!(busy, 0, "no server busy refusals on the happy path");
    assert_eq!(unreadable, 0, "no unreadable objects on the happy path");
    assert_eq!(
        cancelled, 0,
        "no duplicate or abandoned downloads on the happy path"
    );
    tracing::info!(
        transport_bytes,
        expected_bytes,
        requests_a,
        responses_a,
        "snapshot transport traffic"
    );
    Ok(())
}

fn start_node(dir: TempDir) -> Result<TestChild<TempDir>> {
    dir.spawn_child(args!["start"])?.with_timeout(TEST_TIMEOUT)
}

fn stop_node(mut child: TestChild<TempDir>) -> Result<()> {
    child.kill(false)?;
    child
        .wait_with_output()?
        .assert_failure()?
        .assert_was_killed()?;
    Ok(())
}

/// A pruned node bootstraps from a state snapshot, serves and spends from it, keeps it across a
/// restart, and is refused when its checkpoints do not cover the snapshot height.
#[ignore = "runs two nodes through a multi-minute snapshot bootstrap; run it with \
            --run-ignored=all --ignore-default-filter"]
#[tokio::test(flavor = "multi_thread")]
async fn pruned_node_bootstraps_from_a_state_snapshot() -> Result<()> {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(TEST_TIMEOUT, async {
        let network = regtest(None);
        let key_a = TransparentSigningKey::random();
        let address_a = key_a.address(network.kind());

        // Logs on the mining node are filtered to warnings, so reserve every port up front instead
        // of reading the OS-assigned ports from the startup logs.
        let rpc_a = reserve_tcp_port()?;
        let p2p_a = loopback(reserve_tcp_port()?);
        // The QUIC port differs from the TCP port: peers are keyed by address and port across both
        // protocols, so one number for both would make B drop its version 2 connection to A.
        let quic_a = loopback(reserve_udp_port()?);
        let metrics_a = loopback(reserve_tcp_port()?);
        let rpc_b = reserve_tcp_port()?;
        let p2p_b = loopback(reserve_tcp_port()?);
        let quic_b = loopback(reserve_udp_port()?);
        let metrics_b = loopback(reserve_tcp_port()?);
        let rpc_c = reserve_tcp_port()?;
        let p2p_c = loopback(reserve_tcp_port()?);

        // Node A, phase 1: mine until block `H` is finalized, then stop there, so the finalized
        // tip is exactly the snapshot height the exporter requires.
        let mut config_a = node_config(&network, rpc_a, p2p_a, RETENTION_A, &key_a)?;
        config_a.state.debug_stop_at_height = Some(H);
        // Avoid filling the child's log pipe while generating thousands of blocks, but keep the
        // state's stop-at-height line.
        config_a.tracing.filter =
            Some("warn,zebra_state::service::finalized_state=info".to_owned());
        let mut node_a = start_node(testdir()?.with_config(&mut config_a)?)?;
        let dir_a = node_a
            .dir
            .take()
            .expect("retain the state directory for the export");
        let a = RpcRequestClient::new_with_timeout(loopback(rpc_a), TEST_TIMEOUT);
        wait_for_mempool_activation(&a).await?;

        // A coinbase output far below the window, spent by B at the end of the test.
        a.generate(1).await?;
        let early = a
            .get_block(1)
            .await
            .map_err(|err| eyre!(err))?
            .expect("the first generated block exists");
        let coinbase = &early.transactions[0];
        let (index, output) = coinbase
            .outputs()
            .into_iter()
            .enumerate()
            .find(|(_, output)| output.lock_script == address_a.script())
            .expect("the coinbase pays the configured key");
        let outpoint = OutPoint {
            hash: coinbase.hash(),
            index: u32::try_from(index)?,
        };
        let output_params = json!([outpoint.hash.to_string(), outpoint.index, false]).to_string();

        // Block `H` is finalized when the best tip is `H + MAX_BLOCK_REORG_HEIGHT`, which exits A,
        // possibly before the last `generate` call is answered. The stop line is what matters.
        if let Err(error) = mine_to(&a, H + MAX_BLOCK_REORG_HEIGHT).await {
            tracing::info!(%error, "the last generate call lost the stopping node");
        }
        node_a.expect_stdout_line_matches("stopping at configured height")?;
        node_a.kill(true)?;
        node_a.wait_with_output()?;

        // Export the snapshot at `H` from the stopped node's state, into its artifact store.
        let mut export = dir_a
            .spawn_child(args!["export-state-snapshot", "--height": H.to_string()])?
            .with_timeout(TEST_TIMEOUT)?;
        let dir_a = export
            .dir
            .take()
            .expect("retain the state directory for serving");
        let export = export.wait_with_output()?.assert_success()?;
        let commitment = parse_commitment(&String::from_utf8_lossy(&export.output.stdout))?;
        assert_eq!(commitment.height, Height(H));
        let objects_a = ArtifactStore::new(&config_a.state, &network).objects_dir();
        assert!(
            objects_a.join(commitment.manifest.to_string()).is_file(),
            "the exporter stores the manifest under its hash in {}",
            objects_a.display(),
        );

        // Node A, phase 2: serve the snapshot over version 2, and mine past `H + 1000` so B has
        // checkpoints above `H` to verify and blocks above its last checkpoint to verify
        // semantically.
        config_a.state.debug_stop_at_height = None;
        config_a.sync.serve_state_snapshots = true;
        config_a.metrics.endpoint_addr = Some(metrics_a);
        config_a.network.v2_listen_addr = Some(quic_a);
        // B is more than the near-tip window behind A, so its outbound sync dialers require
        // NODE_NETWORK, which pruned A does not advertise. A dials B instead; inbound connections
        // do not have that service requirement, and B uses them to download blocks above H.
        config_a.network.initial_testnet_peers = [p2p_b.to_string()].into();
        config_a.network.initial_v2_peers = [quic_b.to_string()].into();
        config_a.tracing.filter = Some("info".to_owned());
        let mut node_a = start_node(dir_a.replace_config(&mut config_a)?)?;
        let mut dir_a = node_a
            .dir
            .take()
            .expect("retain the state directory for the restart");
        wait_for_mempool_activation(&a).await?;
        // A restarts at `H`, or at `H + MAX_BLOCK_REORG_HEIGHT` with its non-finalized backup;
        // either way it mines until its best tip is past B's last checkpoint.
        let tip_a = block_count(&a)
            .await
            .ok_or_else(|| eyre!("A restarted without a tip"))?
            .max(H + BLOCKS_ABOVE_H + SEMANTIC_TAIL);
        mine_to(&a, tip_a).await?;
        assert_eq!(block_hash(&a, H).await?, commitment.block_hash);
        let tip_hash_a = block_hash(&a, tip_a).await?;
        let state_size_a = dir_size(&config_a.state.db_path(
            "state",
            zebra_state::state_database_format_version_in_code().major,
            &network,
        ))?;

        // Regtest checkpoints from A's chain: genesis, `H`, and heights from `H + 2` up to the
        // last finalized height, so `H + 1 <= max_checkpoint_height` holds (design, §2).
        let mut checkpoints = vec![
            (Height(0), network.genesis_hash()),
            (Height(H), commitment.block_hash),
        ];
        let mut height = H + 2;
        while height < H + BLOCKS_ABOVE_H {
            checkpoints.push((Height(height), block_hash(&a, height).await?));
            height += CHECKPOINT_GAP;
        }
        let last_checkpoint = H + BLOCKS_ABOVE_H;
        checkpoints.push((
            Height(last_checkpoint),
            block_hash(&a, last_checkpoint).await?,
        ));
        let network_b = regtest(Some(checkpoints));

        // Node B: empty and pruned, with the exported commitment, downloading from A.
        let key_b = TransparentSigningKey::random();
        let mut config_b = node_config(&network_b, rpc_b, p2p_b, RETENTION_B, &key_b)?;
        // Keep post-checkpoint validation from starving on loaded hosts with many CPU workers.
        config_b.sync.full_verify_concurrency_limit = 1;
        config_b.sync.parallel_cpu_threads = 1;
        config_b.network.initial_testnet_peers = [p2p_a.to_string()].into();
        config_b.network.v2_listen_addr = Some(quic_b);
        config_b.metrics.endpoint_addr = Some(metrics_b);
        config_b.network.initial_v2_peers = [quic_a.to_string()].into();
        config_b.sync.state_snapshot_sync = true;
        config_b.sync.state_snapshot = Some(commitment);
        config_b.sync.state_snapshot_peers = vec![quic_a.to_string()];
        // Keep the downloaded pieces, so the test can measure what was downloaded.
        config_b.sync.serve_state_snapshots = true;
        let mut node_b = start_node(testdir()?.with_config(&mut config_b)?)?;
        let dir_b = node_b
            .dir
            .take()
            .expect("retain the state directory for the restart");
        let b = RpcRequestClient::new_with_timeout(loopback(rpc_b), TEST_TIMEOUT);

        // The stage lines appear in order, from the decision to the last checkpoint.
        for stage in EXPECTED_STAGES {
            node_b.expect_stdout_line_matches(regex::escape(&stage.to_string()))?;
            if stage == Stage::StateInitialized {
                // Capture transport counters before A's restart resets them. Bootstrap has finished,
                // so these counts are stable and exclude ordinary block sync.
                let (metrics_a, metrics_b) =
                    tokio::try_join!(scrape_metrics(metrics_a), scrape_metrics(metrics_b),)?;
                let objects_b = ArtifactStore::new(&config_b.state, &network_b).objects_dir();
                let manifest_bytes = fs::read(objects_b.join(commitment.manifest.to_string()))?;
                let manifest = Manifest::decode(&manifest_bytes, &network_b)?;
                assert_snapshot_traffic(
                    &metrics_a,
                    &metrics_b,
                    u64::try_from(manifest_bytes.len())? + unique_piece_bytes(&manifest),
                )?;

                // A has served the snapshot and B has imported and opened it while A was pruned.
                // Regtest's fixed old block times make every node count as syncing, so a pruned
                // peer cannot satisfy the required NODE_NETWORK service in either direction.
                // Restart A as an archive for the ordinary block-sync and genesis-sync phases.
                stop_node(node_a)?;
                config_a.state.prune_blocks_older_than = None;
                node_a = start_node(dir_a.replace_config(&mut config_a)?)?;
                dir_a = node_a
                    .dir
                    .take()
                    .expect("retain the state directory for the rest of the test");
                wait_for_mempool_activation(&a).await?;
            }
        }
        wait_for_height(&b, tip_a, SYNC_TIMEOUT, "node B").await?;
        let info = blockchain_info(&b).await?;
        assert_eq!(info["blocks"], tip_a);
        assert_eq!(info["bestblockhash"], tip_hash_a.to_string());

        // Pruned history: the window `L..=H` and everything above it is complete, the heights
        // below `L` have no bodies, and the retained floor is the window start.
        assert_eq!(info["pruned"], true);
        assert_eq!(info["pruneheight"], L);
        for height in [L, H, tip_a] {
            let retained = b
                .get_block(i32::try_from(height)?)
                .await
                .map_err(|err| eyre!(err))?
                .expect("window and synced bodies are retained");
            assert_eq!(retained.coinbase_height(), Some(Height(height)));
        }
        let header_hex: String = b
            .json_result_from_call("getblockheader", json!([H.to_string(), false]).to_string())
            .await
            .map_err(|err| eyre!(err))?;
        let header = block::Header::zcash_deserialize(hex::decode(header_hex)?.as_slice())?;
        assert_eq!(header.hash(), commitment.block_hash);
        for verbosity in [0, 2] {
            let error = get_block_error(&b, L - 1, verbosity).await?;
            // The snapshot ships no body below `L`. Whether the RPC reports it as pruned data
            // (-32603, like a pruned node) or as a block it does not know (-8) depends on the
            // hash rows the snapshot carries, which the design leaves to the importer.
            assert!(
                error["code"] == -32603 && error["message"] == "Block not available (pruned data)"
                    || error["code"] == -8,
                "unexpected getblock error below the window: {error}",
            );
        }

        // Retained file sizes are independent of the transport counters checked above. Identical
        // manifest entries share one object file. The size ratio against A's state is only logged:
        // it is meaningful on Mainnet, not for a Regtest chain whose window is most of the chain.
        let objects_b = ArtifactStore::new(&config_b.state, &network_b).objects_dir();
        let manifest_path = objects_b.join(commitment.manifest.to_string());
        assert!(
            manifest_path.is_file(),
            "B keeps the manifest it bootstrapped from, because it serves snapshots",
        );
        let manifest_bytes = fs::read(&manifest_path)?;
        let manifest = Manifest::decode(&manifest_bytes, &network_b)?;
        assert_eq!(manifest.height, Height(H));
        assert_eq!(manifest.block_hash, commitment.block_hash);
        assert_eq!(manifest.window_start, Height(L));
        let retained_bytes = dir_size(&objects_b)?;
        tracing::info!(
            retained_bytes,
            state_size_a,
            "retained snapshot bytes against A's state directory"
        );
        assert_eq!(
            retained_bytes,
            u64::try_from(manifest_bytes.len())? + unique_piece_bytes(&manifest),
            "B's store holds exactly the manifest and its distinct pieces",
        );

        // Spending a UTXO created below the window: the output is trusted on the manifest, its
        // creating transaction is not available, and the spend is accepted and mined on B.
        let unspent: Value = b
            .json_result_from_call("gettxout", &output_params)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(unspent.get("version"), Some(&Value::Null));
        assert_eq!(unspent["coinbase"], true);
        // The consensus amount limit fits in f64's 53-bit significand, so this cast is exact.
        let expected_zec = u64::from(output.value()) as f64 / 100_000_000.0;
        assert_eq!(unspent["value"], json!(expected_zec));
        let spend = Transaction::signed_transparent(
            &network,
            Height(tip_a + 1),
            &key_a,
            &[(outpoint, output)],
            &[(address_a, Amount::try_from(SPEND_ZATOSHIS)?)],
        )
        .map_err(|err| eyre!(err))?;
        let raw = hex::encode(spend.zcash_serialize_to_vec()?);
        let sent: SendRawTransactionResponse = b
            .json_result_from_call("sendrawtransaction", json!([raw]).to_string())
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(sent.hash(), spend.hash());
        // Long polling waits for template updates, not a guessed delay after mempool admission.
        let template: BlockTemplateResponse =
            tokio::time::timeout(Duration::from_secs(30), async {
                let mut params = "[]".to_owned();
                loop {
                    let template: BlockTemplateResponse = b
                        .json_result_from_call("getblocktemplate", params)
                        .await
                        .map_err(|err| eyre!(err))?;
                    if template
                        .transactions()
                        .iter()
                        .any(|tx| tx.hash() == spend.hash())
                    {
                        return Ok::<_, color_eyre::Report>(template);
                    }
                    params =
                        json!([{"longpollid": template.long_poll_id().to_string()}]).to_string();
                }
            })
            .await
            .map_err(|_| eyre!("B's template did not include the pre-window spend"))??;
        assert_eq!(template.height(), tip_a + 1);
        assert_eq!(template.previous_block_hash(), tip_hash_a);
        // Regtest disables PoW; submitting the populated template exercises full block validation.
        let spend_block = proposal_block_from_template(&template, None, &network_b)?;
        let spend_block_hash = spend_block.hash();
        b.submit_block(spend_block).await?;
        let tip = tip_a + 1;
        let mined = b
            .get_block(i32::try_from(tip)?)
            .await
            .map_err(|err| eyre!(err))?
            .expect("the accepted block is readable");
        assert_eq!(mined.hash(), spend_block_hash);
        assert!(mined
            .transactions
            .iter()
            .any(|tx| tx.hash() == spend.hash()));
        let spent: Value = b
            .json_result_from_call("gettxout", &output_params)
            .await
            .map_err(|err| eyre!(err))?;
        assert_eq!(spent, Value::Null);
        // A accepts the block too, by gossip from B. Submitting it to A directly would race A's own
        // verification of the gossiped block, which `submitblock` reports as a rejection.
        wait_for_height(&a, tip, RELAY_TIMEOUT, "node A").await?;
        assert_eq!(block_hash(&a, tip).await?, spend_block_hash);

        // A restart keeps the imported state and does not bootstrap again: the marker holds.
        stop_node(node_b)?;
        let mut node_b = start_node(dir_b)?.with_failure_regex_set(
            regex::escape(&Stage::BootstrapStarted.to_string()),
            regex::RegexSet::empty(),
        );
        let dir_b = node_b
            .dir
            .take()
            .expect("retain the state directory until the end");
        node_b.expect_stdout_line_matches(regex::escape(&SkipReason::ExistingState.to_string()))?;
        wait_for_mempool_activation(&b).await?;
        wait_for_height(&b, tip, SYNC_TIMEOUT, "node B after its restart").await?;
        let restarted = blockchain_info(&b).await?;
        assert_eq!(restarted["blocks"], tip);
        assert_eq!(restarted["bestblockhash"], spend_block_hash.to_string());
        assert_eq!(restarted["pruned"], true);
        assert_eq!(restarted["pruneheight"], L);
        stop_node(node_b)?;
        drop(dir_b);

        // A second empty node with B's config but without checkpoints is refused by the decision
        // rule, because `H + 1` is above the last checkpoint, and syncs from genesis instead,
        // verifying every block semantically.
        let mut config_c = node_config(&network, rpc_c, p2p_c, RETENTION_B, &key_b)?;
        config_c.sync.full_verify_concurrency_limit = 1;
        config_c.sync.parallel_cpu_threads = 1;
        config_c.sync.state_snapshot_sync = true;
        config_c.sync.state_snapshot = Some(commitment);
        config_c.sync.state_snapshot_peers = vec![quic_a.to_string()];
        // Only the decision rule's reason, logged by zebrad, is read from C's output; the rest of
        // its sync from genesis is polled over RPC, so keep its log pipe from filling up.
        config_c.tracing.filter = Some("warn,zebrad=info".to_owned());
        let mut node_c = start_node(testdir()?.with_config(&mut config_c)?)?
            .with_failure_regex_set(
                regex::escape(&Stage::BootstrapStarted.to_string()),
                regex::RegexSet::empty(),
            );
        node_c.expect_stdout_line_matches(regex::escape(
            &SkipReason::PreconditionFailed(Precondition::BelowLastCheckpoint).to_string(),
        ))?;
        let c = RpcRequestClient::new_with_timeout(loopback(rpc_c), TEST_TIMEOUT);
        wait_for_mempool_activation(&c).await?;
        // C is syncing from genesis, so it refuses outbound peers without `NODE_NETWORK`, which
        // the pruned A does not advertise. Restart A dialling C instead: inbound connections are
        // exempt, and A is at its tip, so it keeps a peer that serves nothing.
        stop_node(node_a)?;
        config_a.network.initial_testnet_peers = [p2p_c.to_string()].into();
        let node_a = start_node(dir_a.replace_config(&mut config_a)?)?;
        wait_for_mempool_activation(&a).await?;
        wait_for_height(&c, tip, SYNC_TIMEOUT, "node C").await?;
        let info_c = blockchain_info(&c).await?;
        assert_eq!(info_c["blocks"], tip);
        assert_eq!(info_c["bestblockhash"], spend_block_hash.to_string());
        assert_eq!(
            info_c["pruned"], false,
            "C synced the whole chain and never pruned"
        );

        stop_node(node_a)?;
        stop_node(node_c)?;
        Ok(())
    })
    .await?
}

/// The stages the test waits for are the contract's stages, in the contract's order, and the
/// window start it checks is the design's `L = max(1, H - 2303)`.
#[test]
fn expected_stages_follow_the_contract() {
    let _init_guard = zebra_test::init();

    assert_eq!(EXPECTED_STAGES[..], Stage::ALL[..EXPECTED_STAGES.len()]);
    assert_eq!(Stage::ALL.last(), Some(&Stage::TargetReached));
    assert_eq!(L, 1.max(H - 2303));
}

#[test]
fn parses_prometheus_object_counters() {
    let _init_guard = zebra_test::init();
    let scrape = r#"
# TYPE zebra_net_in_objects_bytes counter
zebra_net_in_objects_bytes_total 123
# TYPE zebra_net_in_requests counter
zebra_net_in_requests_total{command="get-object"} 17
zebra_net_in_requests_total{command="get-blocks"} 99
zebra_net_in_objects_bytes_extra 999
"#;

    assert_eq!(
        metric(scrape, "zebra.net.in.objects.bytes", None).unwrap(),
        Some(123)
    );
    assert_eq!(
        metric(scrape, "zebra.net.in.requests", Some("get-object")).unwrap(),
        Some(17)
    );
    assert_eq!(
        metric(scrape, "zebra.net.in.requests", Some("get-headers")).unwrap(),
        None
    );
    assert_eq!(
        metric(scrape, "zebra.net.in.objects.busy", None).unwrap(),
        None
    );
}
