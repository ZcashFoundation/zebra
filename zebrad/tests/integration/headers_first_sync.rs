//! Headers-first sync between Regtest nodes on loopback.
//!
//! Node S mines a Regtest chain. Node C starts empty, with checkpoints generated from S's chain,
//! and syncs it from S. With `sync.headers_first`, the default, C downloads, checks and stores
//! the header chain before the bodies: checkpoint spans in parallel below the final checkpoint,
//! then one response at a time above it. Its syncer downloads bodies only for stored headers, so
//! C's header tip is never below its block tip. The tests sample both from C's metrics endpoint
//! while it syncs.
//!
//! The runs cover legacy and version 2 peers together, version 2 peers only, which carry headers
//! with `get-headers`, and `headers_first = false`, where the peer set sends the syncer's
//! `getblocks` walk only to legacy peers.
//!
//! Regtest skips proof of work checks, so the Equihash and difficulty checks have unit tests with
//! Mainnet and Testnet vectors instead.
//!
//! The tests run two nodes through more than a thousand blocks, so they are ignored, and the
//! nextest profiles' default filters exclude them even with `--run-ignored`. Run them one at a
//! time, since they reserve loopback ports and then release them:
//!
//! ```console
//! cargo nextest run -p zebrad --release --ignore-default-filter --run-ignored=only -j 1 \
//!     -E 'test(/^integration::headers_first_sync::/)'
//! ```

use std::{net::SocketAddr, time::Duration};

use color_eyre::eyre::{eyre, Result};
use tempfile::TempDir;

use zebra_chain::block::Height;
use zebra_test::command::TestChild;
use zebrad::{components::header_sync::HEADER_SYNC_CAUGHT_UP, config::ZebradConfig};

use crate::{
    common::{config::testdir, launch::wait_for_mempool_activation, launch::ZebradTestDirExt},
    integration::v2_checkpoint_sync::{
        block_count, block_hash, mine_to, node_config, regtest, start_node, stop_node, Ports,
        TEST_TIMEOUT,
    },
};

/// The height of the final checkpoint.
const FINAL_CHECKPOINT: u32 = 1_000;

/// The blocks S mines above the final checkpoint, which C's header chain extends to one
/// response at a time.
const TAIL: u32 = 100;

/// The gap between consecutive checkpoints, so C downloads several header spans at once.
const CHECKPOINT_GAP: u32 = 100;

const _: () = assert!(FINAL_CHECKPOINT.is_multiple_of(CHECKPOINT_GAP));
const _: () = assert!(
    FINAL_CHECKPOINT / CHECKPOINT_GAP > 1,
    "C downloads more than one checkpoint span"
);

/// C reaching S's tip.
const SYNC_TIMEOUT: Duration = Duration::from_secs(600);

/// How often the test samples C's metrics.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

/// The header task's log line when it starts.
const HEADER_SYNC_STARTED: &str = "starting headers-first sync";

/// The Prometheus names of the header and block tip gauges.
const HEADER_TIP_METRIC: &str = "sync_headers_tip_height";
const BLOCK_TIP_METRIC: &str = "zcash_chain_verified_block_height";

/// The peers C connects to.
#[derive(Copy, Clone, Debug)]
enum Peers {
    /// S over both the legacy and the version 2 protocol.
    LegacyAndV2,
    /// S over the version 2 protocol only.
    V2Only,
}

/// S and C, with S serving its chain and C syncing it.
struct Nodes {
    s: TestChild<TempDir>,
    c: TestChild<TempDir>,
    s_ports: Ports,
    c_ports: Ports,
    c_metrics: SocketAddr,
}

/// Mines S's chain, and starts C with checkpoints from it, connected to S through `peers`.
/// `configure` adjusts C's config.
async fn start_nodes(peers: Peers, configure: impl FnOnce(&mut ZebradConfig)) -> Result<Nodes> {
    let s_ports = Ports::reserve()?;
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
    let network = regtest(Some(checkpoints));

    let legacy_peers = match peers {
        Peers::LegacyAndV2 => vec![s_ports.p2p],
        Peers::V2Only => Vec::new(),
    };
    let mut c_config = node_config(
        &network,
        &c_ports,
        &legacy_peers,
        &[s_ports.quic],
        "warn,zebrad::components::header_sync=info",
    )?;
    // The exporter hides its bound address, so the test reserves a port and releases it just
    // before C starts, like the metrics endpoint test.
    let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
    let c_metrics = reservation.local_addr()?;
    c_config.metrics.endpoint_addr = Some(c_metrics);
    configure(&mut c_config);
    let c_dir = testdir()?.with_config(&mut c_config)?;
    drop(reservation);
    let c = start_node(c_dir)?;

    Ok(Nodes {
        s,
        c,
        s_ports,
        c_ports,
        c_metrics,
    })
}

/// The value of the unlabelled metric `name` in a Prometheus text exposition, if it is there.
fn metric(body: &str, name: &str) -> Option<f64> {
    body.lines().find_map(|line| {
        let value = line.strip_prefix(name)?.strip_prefix(' ')?;
        value.trim().parse().ok()
    })
}

/// The tips C's metrics show: the header tip, if headers-first sync has stored one, and the block
/// tip, if the state has one.
#[derive(Copy, Clone, Debug, Default)]
struct Tips {
    headers: Option<f64>,
    blocks: Option<f64>,
}

async fn sample_tips(client: &reqwest::Client, endpoint: SocketAddr) -> Option<Tips> {
    let body = client
        .get(format!("http://{endpoint}"))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    Some(Tips {
        headers: metric(&body, HEADER_TIP_METRIC),
        blocks: metric(&body, BLOCK_TIP_METRIC),
    })
}

/// What sampling C's tips showed while it synced.
#[derive(Debug, Default)]
struct Samples {
    /// Samples that had both tips.
    both: u32,
    /// Samples where the header tip was above the block tip.
    headers_ahead: u32,
    /// The highest header tip.
    max_headers: f64,
}

/// Waits for C to reach S's tip, sampling C's metrics, and checks that C's header tip is never
/// below its block tip in a sample, with `headers_first`.
async fn sync_and_sample(nodes: &Nodes, headers_first: bool) -> Result<Samples> {
    let tip = FINAL_CHECKPOINT + TAIL;
    let c_rpc = nodes.c_ports.rpc_client();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let mut samples = Samples::default();
    tokio::time::timeout(SYNC_TIMEOUT, async {
        loop {
            if let Some(tips) = sample_tips(&client, nodes.c_metrics).await {
                if let Some(headers) = tips.headers {
                    samples.max_headers = samples.max_headers.max(headers);
                }
                if let (Some(headers), Some(blocks)) = (tips.headers, tips.blocks) {
                    samples.both += 1;
                    if headers > blocks {
                        samples.headers_ahead += 1;
                    }
                    if headers_first && headers < blocks {
                        return Err(eyre!(
                            "C committed block {blocks} above its header tip {headers}"
                        ));
                    }
                }
            }

            if block_count(&c_rpc).await.is_some_and(|count| count >= tip) {
                return Ok(());
            }
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| eyre!("C did not reach height {tip} within {SYNC_TIMEOUT:?}"))??;

    let s_rpc = nodes.s_ports.rpc_client();
    for height in [1, FINAL_CHECKPOINT / 2, FINAL_CHECKPOINT, tip] {
        assert_eq!(
            block_hash(&c_rpc, height).await?,
            block_hash(&s_rpc, height).await?,
            "C and S agree on block {height}"
        );
    }

    Ok(samples)
}

/// Checks C's header sync: it started, its header tip reached S's tip, and it ran ahead of the
/// block tip in at least one sample.
fn assert_headers_first(c: &mut TestChild<TempDir>, samples: &Samples) -> Result<()> {
    c.expect_stdout_line_matches(HEADER_SYNC_STARTED)?;
    c.expect_stdout_line_matches(HEADER_SYNC_CAUGHT_UP)?;

    let tip = f64::from(FINAL_CHECKPOINT + TAIL);
    assert!(
        samples.max_headers >= tip,
        "C's header tip reached S's tip {tip}: {samples:?}"
    );
    assert!(
        samples.both > 0,
        "the metrics showed both tips: {samples:?}"
    );
    assert!(
        samples.headers_ahead > 0,
        "C's header tip ran ahead of its block tip: {samples:?}"
    );
    Ok(())
}

async fn run_headers_first(peers: Peers) -> Result<()> {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut nodes = start_nodes(peers, |_| {}).await?;
        let samples = sync_and_sample(&nodes, true).await?;
        assert_headers_first(&mut nodes.c, &samples)?;

        let Nodes { s, c, .. } = nodes;
        stop_node(c)?;
        stop_node(s)?;
        Ok(())
    })
    .await?
}

/// C syncs S's chain headers first from S over both protocols.
#[ignore = "runs two nodes through 1,100 blocks; run manually, see the module docs"]
#[tokio::test(flavor = "multi_thread")]
async fn headers_first_sync_regtest() -> Result<()> {
    let _init_guard = zebra_test::init();
    run_headers_first(Peers::LegacyAndV2).await
}

/// C syncs S's chain headers first from S over the version 2 protocol only, so every header
/// comes from `get-headers`.
#[ignore = "runs two nodes through 1,100 blocks; run manually, see the module docs"]
#[tokio::test(flavor = "multi_thread")]
async fn headers_first_sync_regtest_v2_only() -> Result<()> {
    let _init_guard = zebra_test::init();
    run_headers_first(Peers::V2Only).await
}

/// With `headers_first = false`, C syncs S's chain with the `getblocks` walk, which the peer set
/// sends only to the legacy connection, and never stores a header tip.
#[ignore = "runs two nodes through 1,100 blocks; run manually, see the module docs"]
#[tokio::test(flavor = "multi_thread")]
async fn block_hash_walk_regtest_without_headers_first() -> Result<()> {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let nodes = start_nodes(Peers::LegacyAndV2, |config| {
            config.sync.headers_first = false;
        })
        .await?;
        let samples = sync_and_sample(&nodes, false).await?;
        assert_eq!(
            samples.max_headers, 0.0,
            "C stores no header chain without headers-first sync: {samples:?}"
        );

        let Nodes { s, c, .. } = nodes;
        stop_node(c)?;
        stop_node(s)?;
        Ok(())
    })
    .await?
}
