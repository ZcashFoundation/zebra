use std::time::Duration;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use color_eyre::eyre::{eyre, Result, WrapErr};

use zebra_chain::{
    block::Height,
    parameters::{
        testnet::{self, ConfiguredActivationHeights},
        Network::*,
    },
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::client::PeerInfo;
use zebra_test::{args, net::random_known_port, prelude::*};

use zebra_network::constants::PORT_IN_USE_ERROR;

use crate::common::{
    config::{default_test_config, testdir},
    launch::{can_spawn_zebrad_for_test_type, ZebradTestDirExt},
    test_type::TestType::*,
};

use crate::integration::database::check_config_conflict;

/// Test will start 2 zebrad nodes one after the other using the same Zcash listener.
/// It is expected that the first node spawned will get exclusive use of the port.
/// The second node will panic with the Zcash listener conflict hint added in #1535.
#[test]
fn zebra_zcash_listener_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created network listen_addr
    let mut config = default_test_config(&Mainnet);
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
#[cfg(feature = "prometheus")]
fn zebra_metrics_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created metrics endpoint_addr
    let mut config = default_test_config(&Mainnet);
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
#[cfg(feature = "filter-reload")]
fn zebra_tracing_conflict() -> Result<()> {
    let _init_guard = zebra_test::init();

    // [Note on port conflict](#Note on port conflict)
    let port = random_known_port();
    let listen_addr = format!("127.0.0.1:{port}");

    // Write a configuration that has our created tracing endpoint_addr
    let mut config = default_test_config(&Mainnet);
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
#[test]
fn zebra_rpc_conflict() -> Result<()> {
    use crate::common::config::random_known_rpc_port_config;

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

/// Check that Zebra will disconnect from misbehaving peers.
///
/// In order to simulate a misbehaviour peer we start two zebrad instances:
/// - The first one is started with a custom Testnet where PoW is disabled.
/// - The second one is started with the default Testnet where PoW is enabled.
/// The second zebrad instance will connect to the first one, and when the first one mines
/// blocks with invalid PoW the second one should disconnect from it.
#[tokio::test]
async fn disconnects_from_misbehaving_peers() -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(10 * 60),
        disconnects_from_misbehaving_peers_impl(),
    )
    .await
    .wrap_err("disconnects_from_misbehaving_peers timed out")?
}

async fn disconnects_from_misbehaving_peers_impl() -> Result<()> {
    use crate::common::regtest::MiningRpcMethods;
    use zebra_rpc::client::PeerInfo;

    let _init_guard = zebra_test::init();
    let network1 = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            ..Default::default()
        })
        .expect("failed to set activation heights")
        .with_slow_start_interval(Height::MIN)
        .with_disable_pow(true)
        .clear_checkpoints()
        .expect("failed to clear checkpoints")
        .with_network_name("PoWDisabledTestnet")
        .expect("failed to set network name")
        .to_network()
        .expect("failed to build configured network");

    let test_type = LaunchWithEmptyState {
        launches_lightwalletd: false,
    };
    let test_name = "disconnects_from_misbehaving_peers_test";

    if !can_spawn_zebrad_for_test_type(test_name, test_type, false) {
        tracing::warn!("skipping disconnects_from_misbehaving_peers test");
        return Ok(());
    }

    // Get the zebrad config
    let mut config = test_type
        .zebrad_config(test_name, false, None, &network1)
        .expect("already checked config")?;

    config.network.cache_dir = false.into();
    config.network.listen_addr = format!("127.0.0.1:{}", random_known_port()).parse()?;
    config.state.ephemeral = true;
    config.network.initial_testnet_peers = [].into();
    config.network.crawl_new_peer_interval = Duration::from_secs(5);

    let rpc_listen_addr = config.rpc.listen_addr.unwrap();
    let rpc_client_1 = RpcRequestClient::new(rpc_listen_addr);

    tracing::info!(
        ?rpc_listen_addr,
        network_listen_addr = ?config.network.listen_addr,
        "starting a zebrad child on incompatible custom Testnet"
    );

    let is_finished = Arc::new(AtomicBool::new(false));
    let _finish_guard = FinishOnDrop::new(Arc::clone(&is_finished));

    let node1_listen_addr = config.network.listen_addr;
    let (node1_listening_tx, node1_listening_rx) = tokio::sync::oneshot::channel::<()>();

    {
        let is_finished = Arc::clone(&is_finished);
        let config = config.clone();
        let (zebrad_failure_messages, zebrad_ignore_messages) = test_type.zebrad_failure_messages();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut zebrad_child = testdir()?
                .with_exact_config(&config)?
                .spawn_child(args!["start"])?
                .bypass_test_capture(true)
                .with_timeout(test_type.zebrad_timeout())
                .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

            // Signal once node 1 is listening. A dial that lands before the listener
            // is open marks node 1 unreachable in node 2's address book, which then
            // never retries it, so node 2 never peers and the test times out.
            zebrad_child.expect_stdout_line_matches(regex::escape(&format!(
                "Opened Zcash protocol endpoint at {node1_listen_addr}"
            )))?;
            let _ = node1_listening_tx.send(());

            while !is_finished.load(Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraA1".to_string()));
            }

            Ok(())
        });
    }

    // Only let node 2 dial once node 1 is listening.
    tokio::time::timeout(Duration::from_secs(60), node1_listening_rx)
        .await
        .wrap_err("timed out waiting for node 1 to open its network listener")?
        .wrap_err("node 1 exited before opening its network listener")?;

    let network2 = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            ..Default::default()
        })
        .expect("failed to set activation heights")
        .with_slow_start_interval(Height::MIN)
        .clear_checkpoints()
        .expect("failed to clear checkpoints")
        .with_network_name("PoWEnabledTestnet")
        .expect("failed to set network name")
        .to_network()
        .expect("failed to build configured network");

    config.network.network = network2;
    config.network.initial_testnet_peers = [config.network.listen_addr.to_string()].into();
    config.network.listen_addr = "127.0.0.1:0".parse()?;
    config.rpc.listen_addr = Some(format!("127.0.0.1:{}", random_known_port()).parse()?);
    config.network.crawl_new_peer_interval = Duration::from_secs(5);
    config.network.cache_dir = false.into();
    config.state.ephemeral = true;

    let rpc_listen_addr = config.rpc.listen_addr.unwrap();
    let rpc_client_2 = RpcRequestClient::new(rpc_listen_addr);

    tracing::info!(
        ?rpc_listen_addr,
        network_listen_addr = ?config.network.listen_addr,
        "starting a zebrad child on the default Testnet"
    );

    {
        let is_finished = Arc::clone(&is_finished);
        tokio::task::spawn_blocking(move || -> Result<()> {
            let (zebrad_failure_messages, zebrad_ignore_messages) =
                test_type.zebrad_failure_messages();
            let mut zebrad_child = testdir()?
                .with_exact_config(&config)?
                .spawn_child(args!["start"])?
                .bypass_test_capture(true)
                .with_timeout(test_type.zebrad_timeout())
                .with_failure_regex_iter(zebrad_failure_messages, zebrad_ignore_messages);

            while !is_finished.load(Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraB2".to_string()));
            }

            Ok(())
        });
    }

    tracing::info!("checking for peers");

    let peer_info = wait_for_outbound_peer(&rpc_client_2).await?;

    tracing::info!(
        ?peer_info,
        "found peer connection, committing genesis block"
    );

    let genesis_block = network1.block_parsed_iter().next().unwrap();
    rpc_client_1.submit_block(genesis_block.clone()).await?;
    rpc_client_2.submit_block(genesis_block).await?;

    // Call the `generate` method to mine blocks in the zebrad instance where PoW is disabled
    tracing::info!("committed genesis block, mining blocks with invalid PoW");
    tokio::time::sleep(Duration::from_secs(2)).await;

    rpc_client_1.call("generate", "[500]").await?;

    tracing::info!("wait for misbehavior messages to flush into address updater channel");

    tokio::time::sleep(Duration::from_secs(30)).await;

    tracing::info!("calling getpeerinfo to confirm Zebra has dropped the peer connection");

    // Call `getpeerinfo` to check that the zebrad instances have disconnected
    for i in 0..600 {
        let peer_info: Vec<PeerInfo> = rpc_client_2
            .json_result_from_call("getpeerinfo", "[]")
            .await
            .map_err(|err| eyre!(err))?;

        if peer_info.is_empty() {
            break;
        } else if i % 10 == 0 {
            tracing::info!(?peer_info, "has not yet disconnected from misbehaving peer");
        }

        rpc_client_1.call("generate", "[1]").await?;

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let peer_info: Vec<PeerInfo> = rpc_client_2
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;

    tracing::info!(?peer_info, "called getpeerinfo");

    assert!(peer_info.is_empty(), "should have no peers");

    is_finished.store(true, Ordering::SeqCst);

    Ok(())
}

/// Regression test for #10715: a node that has synced to the shared chain tip must not
/// disconnect its upstream peer due to empty `FindBlocks` responses.
///
/// Before the fix, the stall tracker ran unconditionally and disconnected peers after 3
/// consecutive empty-response `FindBlocks` cycles, even when both nodes were already at
/// the same tip. The fix gates stall tracking on `is_at_or_near_network_tip`, so the
/// peer is only tracked during genuine catch-up, not once the node is fully synced.
///
/// Sets up two regtest nodes:
/// - Node A (upstream): mines a small chain and listens for P2P connections
/// - Node B (internal): connects exclusively to Node A and syncs to its tip
///
/// After both nodes share the same tip, the test polls `getpeerinfo` on Node B for
/// 30 seconds and asserts that Node A is always connected. If the stall tracker fires
/// incorrectly, Node A is dropped within ~6 seconds (3 cycles × 2-second regtest restart
/// delay) and the assertion fails.
#[tokio::test]
async fn synced_node_keeps_upstream_peer_connected() -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(5 * 60),
        synced_node_keeps_upstream_peer_connected_impl(),
    )
    .await
    .wrap_err("synced_node_keeps_upstream_peer_connected timed out")?
}

async fn synced_node_keeps_upstream_peer_connected_impl() -> Result<()> {
    use zebra_chain::parameters::Network;
    use zebra_rpc::{client::PeerInfo, server::OPENED_RPC_ENDPOINT_MSG};

    use crate::common::{
        config::{os_assigned_rpc_port_config, read_listen_addr_from_logs},
        launch::LAUNCH_DELAY,
        regtest::MiningRpcMethods,
    };

    let _init_guard = zebra_test::init();

    let net = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );

    // Use a preselected port for Node A's P2P listener so Node B's config can
    // reference it without having to parse Node A's logs.
    let upstream_p2p_addr = format!("127.0.0.1:{}", random_known_port());

    let mut upstream_config = os_assigned_rpc_port_config(false, &net)?;
    upstream_config.network.listen_addr = upstream_p2p_addr.parse()?;
    upstream_config.network.initial_testnet_peers = [].into();
    upstream_config.mempool.debug_enable_at_height = Some(0);

    let mut upstream = testdir()?
        .with_config(&mut upstream_config)?
        .spawn_child(args!["start"])?;

    let upstream_rpc_addr = read_listen_addr_from_logs(&mut upstream, OPENED_RPC_ENDPOINT_MSG)?;

    tokio::time::sleep(LAUNCH_DELAY).await;

    let upstream_client = RpcRequestClient::new(upstream_rpc_addr);

    const BLOCKS_TO_MINE: u32 = 100;
    for _ in 0..BLOCKS_TO_MINE {
        let (block, _height) = upstream_client.block_from_template(&net).await?;
        upstream_client.submit_block(block).await?;
    }

    // Node B connects only to Node A; limit target size to 1 to avoid crawling.
    let mut internal_config = os_assigned_rpc_port_config(false, &net)?;
    internal_config.network.initial_testnet_peers = [upstream_p2p_addr].into();
    internal_config.network.peerset_initial_target_size = 1;

    let mut internal = testdir()?
        .with_config(&mut internal_config)?
        .spawn_child(args!["start"])?;

    let internal_rpc_addr = read_listen_addr_from_logs(&mut internal, OPENED_RPC_ENDPOINT_MSG)?;

    let internal_client = RpcRequestClient::new(internal_rpc_addr);

    // Wait for Node B to connect to Node A.
    wait_for_outbound_peer(&internal_client).await?;

    // Wait for Node B to sync to Node A's tip.
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let info = internal_client.blockchain_info().await?;
            if info.blocks() >= Height(BLOCKS_TO_MINE) {
                return Ok::<_, color_eyre::eyre::Report>(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .wrap_err("timed out waiting for Node B to sync to Node A's tip")??;

    tracing::info!("Node B has synced to Node A's tip; verifying Node A stays connected");

    // The regtest sync loop restarts every 2 seconds. With the bug, 3 consecutive
    // empty-response cycles (~6 seconds) would disconnect Node A. Poll for 30 seconds
    // to give the stall tracker ample time to fire if the fix is not in effect.
    for i in 0..30_u32 {
        let peer_info: Vec<PeerInfo> = internal_client
            .json_result_from_call("getpeerinfo", "[]")
            .await
            .map_err(|err| eyre!(err))?;

        assert!(
            !peer_info.is_empty(),
            "upstream peer was disconnected {i} seconds after reaching the shared tip; \
             the stall tracker must not fire when the node is at or near the network tip"
        );

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    upstream.kill(false)?;
    internal.kill(false)?;

    upstream
        .wait_with_output()?
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    internal
        .wait_with_output()?
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}

async fn wait_for_outbound_peer(rpc_client: &RpcRequestClient) -> Result<Vec<PeerInfo>> {
    tokio::time::timeout(Duration::from_secs(2 * 60), async {
        loop {
            match rpc_client
                .json_result_from_call::<Vec<PeerInfo>>("getpeerinfo", "[]")
                .await
            {
                Ok(peer_info) if !peer_info.is_empty() => {
                    return Ok::<Vec<PeerInfo>, color_eyre::eyre::Report>(peer_info);
                }
                Ok(_) => {
                    tracing::info!("waiting for outbound peer");
                }
                Err(err) => {
                    tracing::info!(?err, "waiting for RPC endpoint and outbound peer");
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .wrap_err("timed out waiting for outbound peer")?
}

struct FinishOnDrop(Arc<AtomicBool>);

impl FinishOnDrop {
    fn new(is_finished: Arc<AtomicBool>) -> Self {
        Self(is_finished)
    }
}

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
