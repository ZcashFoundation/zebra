use std::time::Duration;

use color_eyre::eyre::{eyre, Result};

use zebra_chain::{
    block::Height,
    parameters::{
        testnet::{self, ConfiguredActivationHeights},
        Network::*,
    },
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_test::{args, net::random_known_port, prelude::*};

#[cfg(not(target_os = "windows"))]
use zebra_network::constants::PORT_IN_USE_ERROR;

use crate::common::{
    config::{default_test_config, testdir},
    launch::{can_spawn_zebrad_for_test_type, ZebradTestDirExt, LAUNCH_DELAY},
    test_type::TestType::*,
};

use crate::integration::database::check_config_conflict;

/// Make sure `lightwalletd` works with Zebra, when both their states are empty.
///
/// This test only runs when the `TEST_LIGHTWALLETD` env var is set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lwd_integration() -> Result<()> {
    crate::common::lightwalletd::lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })
}

// NOTE: lwd_integration_test is intentionally not here. See
// crate::common::lightwalletd::lwd_integration_test for the shared helper.
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
#[cfg(not(target_os = "windows"))]
async fn disconnects_from_misbehaving_peers() -> Result<()> {
    use std::sync::{atomic::AtomicBool, Arc};

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

            while !is_finished.load(std::sync::atomic::Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraA1".to_string()));
            }

            Ok(())
        });
    }

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

            while !is_finished.load(std::sync::atomic::Ordering::SeqCst) {
                zebrad_child.wait_for_stdout_line(Some("zebraB2".to_string()));
            }

            Ok(())
        });
    }

    tracing::info!("waiting for zebrad nodes to connect");

    // Wait a few seconds for Zebra to start up and make outbound peer connections
    tokio::time::sleep(LAUNCH_DELAY).await;

    tracing::info!("checking for peers");

    // Call `getpeerinfo` to check that the zebrad instances have connected
    let peer_info: Vec<PeerInfo> = rpc_client_2
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|err| eyre!(err))?;

    assert!(!peer_info.is_empty(), "should have outbound peer");

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

    is_finished.store(true, std::sync::atomic::Ordering::SeqCst);

    Ok(())
}
