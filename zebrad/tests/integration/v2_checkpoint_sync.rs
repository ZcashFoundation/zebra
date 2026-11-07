//! Checkpoint sync over the version 2 protocol, between Regtest nodes on loopback.
//!
//! Node S mines a Regtest chain. Node S2 syncs it from S. Both serve `get-block-range` over the
//! version 2 protocol. Node C starts empty, with checkpoints generated from S's chain and
//! `sync.v2_checkpoint_sync` set: the checkpoint sync engine takes block download over from the
//! legacy syncer, hands it back at a checkpoint, and the legacy syncer finishes the blocks above
//! the final checkpoint. A second run stops a server while the engine runs, and C falls back to
//! the legacy syncer. A third run gives C known-hash pins for S's chain, and the engine anchors
//! its downloads at known hashes it fetches from S and S2 with `get-hashes`.
//!
//! The tests wait for the syncer's ownership log lines, which `zebrad::components::sync` writes.
//! They run three nodes through thousands of blocks, so they are ignored, and the nextest
//! profiles' default filters exclude them even with `--run-ignored`, as CI runs ignored tests.
//! Run them one at a time, since they reserve loopback ports and then release them:
//!
//! ```console
//! cargo nextest run -p zebrad --release --ignore-default-filter --run-ignored=only -j 1 \
//!     -E 'test(/^integration::v2_checkpoint_sync::/)'
//! ```

use std::{
    net::{SocketAddr, TcpListener, UdpSocket},
    time::Duration,
};

use color_eyre::eyre::{eyre, Result};
use regex::Regex;
use tempfile::TempDir;

use zebra_chain::{
    block::{self, Block, Height, SyncHashEntry},
    parameters::{
        known_hashes::chunk_pin,
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    serialization::ZcashDeserializeInto,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_test::{args, command::TestChild, prelude::*};
use zebrad::{
    components::checkpoint_sync::known_hashes::{ChunkPin, KnownHashPins},
    config::ZebradConfig,
};

use crate::common::{
    config::{rpc_port_config, testdir},
    launch::{wait_for_mempool_activation, ZebradTestDirExt},
    regtest::MiningRpcMethods,
};

/// The height of the final checkpoint.
const FINAL_CHECKPOINT: u32 = 2_800;

/// The blocks S mines above the final checkpoint, which the legacy syncer verifies
/// semantically after the hand-off.
const TAIL: u32 = 200;

/// The gap between consecutive checkpoints. The engine claims only a gap of about 20 checkpoint
/// intervals, so the chain has many more than that.
const CHECKPOINT_GAP: u32 = 100;

const _: () = assert!(FINAL_CHECKPOINT.is_multiple_of(CHECKPOINT_GAP));
const _: () = assert!(
    FINAL_CHECKPOINT / CHECKPOINT_GAP > 20,
    "the gap is large enough for the engine to claim block download"
);
const _: () = assert!(
    // u32 to usize is a lossless widening on every target Zebra supports.
    CHECKPOINT_GAP as usize <= zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP,
    "checkpoint gaps fit the verifier's download window"
);

/// One whole run.
pub(crate) const TEST_TIMEOUT: Duration = Duration::from_secs(900);

/// One node reaching a height.
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

/// Blocks per `generate` call, so each RPC call stays well inside the client timeout.
const GENERATE_BATCH: u32 = 500;

/// The syncer's log line when the engine owns block download.
const ENGINE_OWNS: &str = "the checkpoint sync engine owns block download";

/// The syncer's log line when the engine hands block download back.
const ENGINE_HANDED_BACK: &str = "the checkpoint sync engine handed block download back";

/// The supervisor's log line when the engine hands block download back, with the number of
/// units it anchored at known hashes.
const SUPERVISOR_HANDED_BACK: &str =
    "checkpoint sync engine handed block download back to the legacy syncer";

/// The highest height of the known-hash list, one chunk. Nodes serve `get-hashes` entries only
/// up to `MAX_BLOCK_REORG_HEIGHT` below their tips, so the list ends well below S's tip minus
/// that margin.
const KNOWN_HASH_MAX_HEIGHT: u32 = 1_900;

const _: () = assert!(
    KNOWN_HASH_MAX_HEIGHT + zebra_state::MAX_BLOCK_REORG_HEIGHT < FINAL_CHECKPOINT + TAIL,
    "S and S2 serve every entry of the list"
);

fn reserve_tcp_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn reserve_udp_port() -> Result<u16> {
    Ok(UdpSocket::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn loopback(port: u16) -> SocketAddr {
    ([127, 0, 0, 1], port).into()
}

/// Regtest with NU5 from height 1, and `checkpoints`, or genesis only for `None`.
pub(crate) fn regtest(checkpoints: Option<Vec<(Height, block::Hash)>>) -> Network {
    Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        },
        checkpoints: checkpoints.map(ConfiguredCheckpoints::HeightsAndHashes),
        ..Default::default()
    })
}

/// The addresses of one node.
pub(crate) struct Ports {
    pub(crate) rpc: u16,
    pub(crate) p2p: SocketAddr,
    pub(crate) quic: SocketAddr,
}

impl Ports {
    pub(crate) fn reserve() -> Result<Self> {
        Ok(Ports {
            rpc: reserve_tcp_port()?,
            p2p: loopback(reserve_tcp_port()?),
            quic: loopback(reserve_udp_port()?),
        })
    }

    pub(crate) fn rpc_client(&self) -> RpcRequestClient {
        RpcRequestClient::new_with_timeout(loopback(self.rpc), TEST_TIMEOUT)
    }
}

/// A version 2 node on `network`, with legacy peers `legacy_peers` and version 2 peers
/// `v2_peers`.
///
/// The crawler interval is long, so loopback nodes do not open connections beyond the
/// configured ones.
pub(crate) fn node_config(
    network: &Network,
    ports: &Ports,
    legacy_peers: &[SocketAddr],
    v2_peers: &[SocketAddr],
    log_filter: &str,
) -> Result<ZebradConfig> {
    let mut config = rpc_port_config(ports.rpc, false, network)?;
    config.network.listen_addr = ports.p2p;
    config.network.v2_listen_addr = Some(ports.quic);
    config.network.initial_testnet_peers = legacy_peers.iter().map(ToString::to_string).collect();
    config.network.initial_v2_peers = v2_peers.iter().map(ToString::to_string).collect();
    config.network.max_connections_per_ip = 4;
    config.network.crawl_new_peer_interval = Duration::from_secs(3600);
    config.mempool.debug_enable_at_height = Some(0);
    config.tracing.filter = Some(log_filter.to_owned());
    Ok(config)
}

pub(crate) fn start_node(dir: TempDir) -> Result<TestChild<TempDir>> {
    dir.spawn_child(args!["start"])?.with_timeout(TEST_TIMEOUT)
}

pub(crate) fn stop_node(mut child: TestChild<TempDir>) -> Result<()> {
    child.kill(false)?;
    child
        .wait_with_output()?
        .assert_failure()?
        .assert_was_killed()?;
    Ok(())
}

pub(crate) async fn block_count(client: &RpcRequestClient) -> Option<u32> {
    client
        .json_result_from_call::<u32>("getblockcount", "[]")
        .await
        .ok()
}

pub(crate) async fn wait_for_height(
    client: &RpcRequestClient,
    height: u32,
    node: &str,
) -> Result<()> {
    tokio::time::timeout(SYNC_TIMEOUT, async {
        loop {
            if block_count(client)
                .await
                .is_some_and(|count| count >= height)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| eyre!("{node} did not reach height {height} within {SYNC_TIMEOUT:?}"))
}

pub(crate) async fn mine_to(client: &RpcRequestClient, best_tip: u32) -> Result<()> {
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

/// The known-hash list pins of S's chain up to `max_height`, from S's blocks.
async fn known_hash_pins(client: &RpcRequestClient, max_height: u32) -> Result<KnownHashPins> {
    let mut entries = Vec::new();
    for height in 0..=max_height {
        let raw = hex::decode(
            client
                .json_result_from_call::<String>("getblock", format!("[\"{height}\", 0]"))
                .await
                .map_err(|err| eyre!(err))?,
        )?;
        let block: Block = raw.zcash_deserialize_into()?;
        entries.push(SyncHashEntry::for_block(&block, raw.len()));
    }

    // A list below 50,000 heights is one chunk.
    Ok(KnownHashPins {
        max_height: Height(max_height),
        chunk_hashes: vec![ChunkPin(chunk_pin(&entries))],
    })
}

pub(crate) async fn block_hash(client: &RpcRequestClient, height: u32) -> Result<block::Hash> {
    Ok(client
        .json_result_from_call::<String>("getblockhash", format!("[{height}]"))
        .await
        .map_err(|err| eyre!(err))?
        .parse()?)
}

/// The three nodes, with S and S2 serving S's chain and C syncing.
struct Nodes {
    s: TestChild<TempDir>,
    s2: TestChild<TempDir>,
    c: TestChild<TempDir>,
    s_rpc: RpcRequestClient,
    c_rpc: RpcRequestClient,
    checkpoint_heights: Vec<u32>,
}

/// Mines S's chain, syncs S2 from S, and starts C, with known-hash pins for S's chain up to
/// `known_hash_max_height` if it is set.
async fn start_nodes(known_hash_max_height: Option<u32>) -> Result<Nodes> {
    let s_ports = Ports::reserve()?;
    let s2_ports = Ports::reserve()?;
    let c_ports = Ports::reserve()?;

    let mut s_config = node_config(&regtest(None), &s_ports, &[], &[], "warn")?;
    let s = start_node(testdir()?.with_config(&mut s_config)?)?;
    let s_rpc = s_ports.rpc_client();
    wait_for_mempool_activation(&s_rpc).await?;
    mine_to(&s_rpc, FINAL_CHECKPOINT + TAIL).await?;

    let mut checkpoints = Vec::new();
    for height in (0..=FINAL_CHECKPOINT).step_by(CHECKPOINT_GAP as usize) {
        checkpoints.push((Height(height), block_hash(&s_rpc, height).await?));
    }
    let checkpoint_heights = checkpoints.iter().map(|(height, _)| height.0).collect();
    let network = regtest(Some(checkpoints));

    // S2 syncs the chain from S over the legacy protocol, so that C has two range servers.
    let mut s2_config = node_config(&network, &s2_ports, &[s_ports.p2p], &[], "warn")?;
    let s2 = start_node(testdir()?.with_config(&mut s2_config)?)?;
    wait_for_height(&s2_ports.rpc_client(), FINAL_CHECKPOINT + TAIL, "S2").await?;

    // C downloads genesis and the tail from S over the legacy protocol, and the checkpointed
    // blocks from S and S2 over version 2.
    let mut c_config = node_config(
        &network,
        &c_ports,
        &[s_ports.p2p],
        &[s_ports.quic, s2_ports.quic],
        "warn,zebrad::components::sync=info,zebrad::components::checkpoint_sync=info",
    )?;
    c_config.sync.v2_checkpoint_sync = true;
    if let Some(max_height) = known_hash_max_height {
        c_config.sync.v2_known_hash_pins = Some(known_hash_pins(&s_rpc, max_height).await?);
    }
    let c = start_node(testdir()?.with_config(&mut c_config)?)?;

    Ok(Nodes {
        s,
        s2,
        c,
        s_rpc,
        c_rpc: c_ports.rpc_client(),
        checkpoint_heights,
    })
}

/// The state tip height in a hand-back log line.
fn handed_back_height(line: &str) -> Result<u32> {
    let height = Regex::new(r"state_tip=Some\(Height\((\d+)\)\)")?
        .captures(line)
        .ok_or_else(|| eyre!("no state tip in {line:?}"))?;
    Ok(height[1].parse()?)
}

/// Checks that C has S's chain up to S's tip.
async fn assert_same_chain(nodes: &Nodes) -> Result<()> {
    let tip = FINAL_CHECKPOINT + TAIL;
    wait_for_height(&nodes.c_rpc, tip, "C").await?;
    for height in [1, FINAL_CHECKPOINT / 2, FINAL_CHECKPOINT, tip] {
        assert_eq!(
            block_hash(&nodes.c_rpc, height).await?,
            block_hash(&nodes.s_rpc, height).await?,
            "C and S agree on block {height}"
        );
    }
    Ok(())
}

/// C syncs the checkpointed chain with the engine, which hands back at a checkpoint, and the
/// legacy syncer finishes the tail.
#[ignore = "runs three nodes through 3,000 blocks; run manually, see the module docs"]
#[tokio::test(flavor = "multi_thread")]
async fn v2_checkpoint_sync_regtest() -> Result<()> {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut nodes = start_nodes(None).await?;

        nodes.c.expect_stdout_line_matches(ENGINE_OWNS)?;
        let line = nodes.c.expect_stdout_line_matches(ENGINE_HANDED_BACK)?;
        let handed_back = handed_back_height(&line)?;
        assert!(
            nodes.checkpoint_heights.contains(&handed_back),
            "the engine hands back at a checkpoint, not at {handed_back}"
        );
        assert!(
            handed_back > CHECKPOINT_GAP,
            "the engine committed blocks before it handed back at {handed_back}"
        );

        assert_same_chain(&nodes).await?;

        let Nodes { s, s2, c, .. } = nodes;
        stop_node(c)?;
        stop_node(s2)?;
        stop_node(s)?;
        Ok(())
    })
    .await?
}

/// When one of C's two range servers stops while the engine runs, the engine hands block
/// download back, and the legacy syncer finishes the chain.
///
/// Over loopback, the engine can reach its target before the server stops, so this test
/// doesn't always exercise the fall-back: it then checks that the legacy syncer finishes the
/// chain with one range server gone.
#[ignore = "runs three nodes through 3,000 blocks; run manually, see the module docs"]
#[tokio::test(flavor = "multi_thread")]
async fn v2_checkpoint_sync_regtest_falls_back_to_legacy() -> Result<()> {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut nodes = start_nodes(None).await?;

        nodes.c.expect_stdout_line_matches(ENGINE_OWNS)?;
        let Nodes {
            s,
            s2,
            mut c,
            s_rpc,
            c_rpc,
            checkpoint_heights,
        } = nodes;
        stop_node(s2)?;

        let line = c.expect_stdout_line_matches(ENGINE_HANDED_BACK)?;
        let handed_back = handed_back_height(&line)?;
        assert!(
            checkpoint_heights.contains(&handed_back),
            "the engine hands back at a checkpoint, not at {handed_back}"
        );

        let tip = FINAL_CHECKPOINT + TAIL;
        wait_for_height(&c_rpc, tip, "C").await?;
        assert_eq!(
            block_hash(&c_rpc, tip).await?,
            block_hash(&s_rpc, tip).await?,
            "C syncs S's chain with the legacy syncer"
        );

        stop_node(c)?;
        stop_node(s)?;
        Ok(())
    })
    .await?
}

/// With known-hash pins for S's chain, C's engine fetches the list from S and S2 with
/// `get-hashes`, verifies it against the pins, and anchors its downloads at known hashes.
#[ignore = "runs three nodes through 3,000 blocks; run manually, see the module docs"]
#[tokio::test(flavor = "multi_thread")]
async fn v2_checkpoint_sync_regtest_anchors_at_known_hashes() -> Result<()> {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut nodes = start_nodes(Some(KNOWN_HASH_MAX_HEIGHT)).await?;

        nodes.c.expect_stdout_line_matches(ENGINE_OWNS)?;
        let line = nodes.c.expect_stdout_line_matches(SUPERVISOR_HANDED_BACK)?;
        let known_hash_units: u64 = Regex::new(r"known_hash_units=(\d+)")?
            .captures(&line)
            .ok_or_else(|| eyre!("no known-hash unit count in {line:?}"))?[1]
            .parse()?;
        assert!(
            known_hash_units > 0,
            "the engine anchored units at known hashes: {line:?}"
        );

        assert_same_chain(&nodes).await?;

        let Nodes { s, s2, c, .. } = nodes;
        stop_node(c)?;
        stop_node(s2)?;
        stop_node(s)?;
        Ok(())
    })
    .await?
}
