//! Lightwallet tests

use std::panic;

use color_eyre::eyre::WrapErr;

use zebra_chain::parameters::Network::{self, *};
use zebra_node_services::rpc_client::RpcRequestClient;

use zebra_state::state_database_format_version_in_code;
use zebra_test::prelude::*;

use crate::common::{
    cached_state::{
        wait_for_state_version_message, wait_for_state_version_upgrade,
        DATABASE_FORMAT_UPGRADE_IS_LONG,
    },
    launch::spawn_zebrad_for_rpc,
    lightwalletd::{can_spawn_lightwalletd_for_rpc, spawn_lightwalletd_for_rpc},
    sync::SYNC_FINISHED_REGEX,
    test_type::TestType::{self, *},
};

/// Make sure `lightwalletd` works with Zebra, when both their states are empty.
///
/// This test only runs when the `TEST_LIGHTWALLETD` env var is set.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
fn lwd_integration() -> Result<()> {
    lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })
}

/// Make sure `zebrad` can sync from peers, but don't actually launch `lightwalletd`.
///
/// This test only runs when a persistent cached state directory path is configured
/// (for example, by setting `ZEBRA_STATE__CACHE_DIR`).
///
/// This test might work on Windows.
#[test]
#[ignore]
fn sync_update_mainnet() -> Result<()> {
    lwd_integration_test(UpdateZebraCachedStateNoRpc)
}

#[tokio::test]
#[ignore]
async fn lwd_rpc_test() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We're only using cached Zebra state here, so this test type is the most similar
    let test_type = TestType::UpdateCachedState;
    let network = Network::Mainnet;

    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) =
        spawn_zebrad_for_rpc(network, "lwd_rpc_test", test_type, false)?
    {
        tracing::info!("running fully synced zebrad RPC test");

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    let zebra_rpc_address = zebra_rpc_address.expect("lightwalletd test must have RPC port");

    zebrad.expect_stdout_line_matches(format!("Opened RPC endpoint at {zebra_rpc_address}"))?;

    let client = RpcRequestClient::new(zebra_rpc_address);

    // Make a getblock test that works only on synced node (high block number).
    // The block is before the mandatory checkpoint, so the checkpoint cached state can be used
    // if desired.
    let res = client
        .text_from_call("getblock", r#"["1180900", 0]"#.to_string())
        .await?;

    // Simple textual check to avoid fully parsing the response, for simplicity
    let expected_bytes = zebra_test::vectors::MAINNET_BLOCKS
        .get(&1_180_900)
        .expect("test block must exist");
    let expected_hex = hex::encode(expected_bytes);
    assert!(
        res.contains(&expected_hex),
        "response did not contain the desired block: {res}"
    );

    Ok(())
}

/// Make sure `lightwalletd` can sync from Zebra, in update sync mode.
///
/// This test only runs when:
/// - `TEST_LIGHTWALLETD` is set,
/// - a persistent cached state directory path is configured (e.g., via `ZEBRA_STATE__CACHE_DIR`), and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[test]
#[cfg(not(target_os = "windows"))]
#[cfg(feature = "lightwalletd-grpc-tests")]
fn lwd_sync_update() -> Result<()> {
    lwd_integration_test(UpdateCachedState)
}

/// Test sending transactions using a lightwalletd instance connected to a zebrad instance.
///
/// See [`common::lightwalletd::send_transaction_test`] for more information.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
#[cfg(not(target_os = "windows"))]
async fn lwd_rpc_send_tx() -> Result<()> {
    common::lightwalletd::send_transaction_test::run().await
}

/// Test all the rpc methods a wallet connected to lightwalletd can call.
///
/// See [`common::lightwalletd::wallet_grpc_test`] for more information.
///
/// This test doesn't work on Windows, so it is always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(feature = "lightwalletd-grpc-tests")]
#[cfg(not(target_os = "windows"))]
async fn lwd_grpc_wallet() -> Result<()> {
    common::lightwalletd::wallet_grpc_test::run().await
}

/// Make sure `lightwalletd` can sync from Zebra, in all available modes.
///
/// Runs the tests in this order:
/// - launch lightwalletd with empty states,
/// - if a cached Zebra state directory path is configured:
///   - run a full sync
/// - if a cached Zebra state directory path is configured:
///   - run a quick update sync,
///   - run a send transaction gRPC test,
///   - run read-only gRPC tests.
///
/// The lightwalletd full, update, and gRPC tests only run with `--features=lightwalletd-grpc-tests`.
///
/// These tests don't work on Windows, so they are always skipped on that platform.
#[tokio::test]
#[ignore]
#[cfg(not(target_os = "windows"))]
async fn lightwalletd_test_suite() -> Result<()> {
    lwd_integration_test(LaunchWithEmptyState {
        launches_lightwalletd: true,
    })?;

    // Only runs when a cached Zebra state directory path is configured with an environment variable.
    lwd_integration_test(UpdateZebraCachedStateNoRpc)?;

    // These tests need the compile-time gRPC feature
    #[cfg(feature = "lightwalletd-grpc-tests")]
    {
        // Do the quick tests first

        // Only runs when a cached Zebra state is configured
        lwd_integration_test(UpdateCachedState)?;

        // Only runs when a cached Zebra state is configured
        common::lightwalletd::wallet_grpc_test::run().await?;

        // Then do the slow tests

        // Only runs when a cached Zebra state is configured.
        // When manually running the test suite, allow cached state in the full sync test.
        lwd_integration_test(FullSyncFromGenesis {
            allow_lightwalletd_cached_state: true,
        })?;

        // Only runs when a cached Zebra state is configured
        common::lightwalletd::send_transaction_test::run().await?;
    }

    Ok(())
}

/// Run a lightwalletd integration test with a configuration for `test_type`.
///
/// Tests that sync `lightwalletd` to the chain tip require the `lightwalletd-grpc-tests` feature`:
/// - [`FullSyncFromGenesis`]
/// - [`UpdateCachedState`]
///
/// Set `FullSyncFromGenesis { allow_lightwalletd_cached_state: true }` to speed up manual full sync tests.
///
/// # Relibility
///
/// The random ports in this test can cause [rare port conflicts.](#Note on port conflict)
///
/// # Panics
///
/// If the `test_type` requires `--features=lightwalletd-grpc-tests`,
/// but Zebra was not compiled with that feature.
#[tracing::instrument]
fn lwd_integration_test(test_type: TestType) -> Result<()> {
    let _init_guard = zebra_test::init();

    // We run these sync tests with a network connection, for better test coverage.
    let use_internet_connection = true;
    let network = Mainnet;
    let test_name = "lwd_integration_test";

    if test_type.launches_lightwalletd() && !can_spawn_lightwalletd_for_rpc(test_name, test_type) {
        tracing::info!("skipping test due to missing lightwalletd network or cached state");
        return Ok(());
    }

    // Launch zebra with peers and using a predefined zebrad state path.
    let (mut zebrad, zebra_rpc_address) = if let Some(zebrad_and_address) = spawn_zebrad_for_rpc(
        network.clone(),
        test_name,
        test_type,
        use_internet_connection,
    )? {
        tracing::info!(
            ?test_type,
            "running lightwalletd & zebrad integration test, launching zebrad...",
        );

        zebrad_and_address
    } else {
        // Skip the test, we don't have the required cached state
        return Ok(());
    };

    // Store the state version message so we can wait for the upgrade later if needed.
    let state_version_message = wait_for_state_version_message(&mut zebrad)?;

    if test_type.needs_zebra_cached_state() {
        zebrad
            .expect_stdout_line_matches(r"loaded Zebra state cache .*tip.*=.*Height\([0-9]{7}\)")?;
    } else {
        // Timeout the test if we're somehow accidentally using a cached state
        zebrad.expect_stdout_line_matches("loaded Zebra state cache .*tip.*=.*None")?;
    }

    // Wait for the state to upgrade and the RPC port, if the upgrade is short.
    //
    // If incompletely upgraded states get written to the CI cache,
    // change DATABASE_FORMAT_UPGRADE_IS_LONG to true.
    if !DATABASE_FORMAT_UPGRADE_IS_LONG {
        if test_type.launches_lightwalletd() {
            tracing::info!(
                ?test_type,
                ?zebra_rpc_address,
                "waiting for zebrad to open its RPC port..."
            );
            wait_for_state_version_upgrade(
                &mut zebrad,
                &state_version_message,
                state_database_format_version_in_code(),
                [format!(
                    "Opened RPC endpoint at {}",
                    zebra_rpc_address.expect("lightwalletd test must have RPC port")
                )],
            )?;
        } else {
            wait_for_state_version_upgrade(
                &mut zebrad,
                &state_version_message,
                state_database_format_version_in_code(),
                None,
            )?;
        }
    }

    // Wait for zebrad to sync the genesis block before launching lightwalletd,
    // if lightwalletd is launched and zebrad starts with an empty state.
    // This prevents lightwalletd from exiting early due to an empty state.
    if test_type.launches_lightwalletd() && !test_type.needs_zebra_cached_state() {
        tracing::info!(
            ?test_type,
            "waiting for zebrad to sync genesis block before launching lightwalletd...",
        );
        // Wait for zebrad to commit the genesis block to the state.
        // Use the syncer's state tip log message, as the specific commit log might not appear reliably.
        zebrad.expect_stdout_line_matches(
            "starting sync, obtaining new tips state_tip=Some\\(Height\\(0\\)\\)",
        )?;
    }

    // Launch lightwalletd, if needed
    let lightwalletd_and_port = if test_type.launches_lightwalletd() {
        tracing::info!(
            ?zebra_rpc_address,
            "launching lightwalletd connected to zebrad",
        );

        // Launch lightwalletd
        let (mut lightwalletd, lightwalletd_rpc_port) = spawn_lightwalletd_for_rpc(
            network,
            test_name,
            test_type,
            zebra_rpc_address.expect("lightwalletd test must have RPC port"),
        )?
        .expect("already checked for lightwalletd cached state and network");

        tracing::info!(
            ?lightwalletd_rpc_port,
            "spawned lightwalletd connected to zebrad",
        );

        // Check that `lightwalletd` is calling the expected Zebra RPCs

        // getblockchaininfo
        if test_type.needs_zebra_cached_state() {
            lightwalletd.expect_stdout_line_matches(
                "Got sapling height 419200 block height [0-9]{7} chain main branchID [0-9a-f]{8}",
            )?;
        } else {
            // Timeout the test if we're somehow accidentally using a cached state in our temp dir
            lightwalletd.expect_stdout_line_matches(
                "Got sapling height 419200 block height [0-9]{1,6} chain main branchID 00000000",
            )?;
        }

        if test_type.needs_lightwalletd_cached_state() {
            lightwalletd
                .expect_stdout_line_matches("Done reading [0-9]{7} blocks from disk cache")?;
        } else if !test_type.allow_lightwalletd_cached_state() {
            // Timeout the test if we're somehow accidentally using a cached state in our temp dir
            lightwalletd.expect_stdout_line_matches("Done reading 0 blocks from disk cache")?;
        }

        // getblock with the first Sapling block in Zebra's state
        //
        // zcash/lightwalletd calls getbestblockhash here, but
        // adityapk00/lightwalletd calls getblock
        //
        // The log also depends on what is in Zebra's state:
        //
        // # Cached Zebra State
        //
        // lightwalletd ingests blocks into its cache.
        //
        // # Empty Zebra State
        //
        // lightwalletd tries to download the Sapling activation block, but it's not in the state.
        //
        // Until the Sapling activation block has been downloaded,
        // lightwalletd will keep retrying getblock.
        if !test_type.allow_lightwalletd_cached_state() {
            if test_type.needs_zebra_cached_state() {
                lightwalletd.expect_stdout_line_matches(
                    "([Aa]dding block to cache)|([Ww]aiting for block)",
                )?;
            } else {
                lightwalletd.expect_stdout_line_matches(regex::escape(
                    "Waiting for zcashd height to reach Sapling activation height (419200)",
                ))?;
            }
        }

        Some((lightwalletd, lightwalletd_rpc_port))
    } else {
        None
    };

    // Wait for zebrad and lightwalletd to sync, if needed.
    let (mut zebrad, lightwalletd) = if test_type.needs_zebra_cached_state() {
        if let Some((lightwalletd, lightwalletd_rpc_port)) = lightwalletd_and_port {
            #[cfg(feature = "lightwalletd-grpc-tests")]
            {
                use common::lightwalletd::sync::wait_for_zebrad_and_lightwalletd_sync;

                tracing::info!(
                    ?lightwalletd_rpc_port,
                    "waiting for zebrad and lightwalletd to sync...",
                );

                let (lightwalletd, mut zebrad) = wait_for_zebrad_and_lightwalletd_sync(
                    lightwalletd,
                    lightwalletd_rpc_port,
                    zebrad,
                    zebra_rpc_address.expect("lightwalletd test must have RPC port"),
                    test_type,
                    // We want to wait for the mempool and network for better coverage
                    true,
                    use_internet_connection,
                )?;

                // Wait for the state to upgrade, if the upgrade is long.
                // If this line hangs, change DATABASE_FORMAT_UPGRADE_IS_LONG to false,
                // or combine "wait for sync" with "wait for state version upgrade".
                if DATABASE_FORMAT_UPGRADE_IS_LONG {
                    wait_for_state_version_upgrade(
                        &mut zebrad,
                        &state_version_message,
                        state_database_format_version_in_code(),
                        None,
                    )?;
                }

                (zebrad, Some(lightwalletd))
            }

            #[cfg(not(feature = "lightwalletd-grpc-tests"))]
            panic!(
                "the {test_type:?} test requires `cargo test --feature lightwalletd-grpc-tests`\n\
                 zebrad: {zebrad:?}\n\
                 lightwalletd: {lightwalletd:?}\n\
                 lightwalletd_rpc_port: {lightwalletd_rpc_port:?}"
            );
        } else {
            // We're just syncing Zebra, so there's no lightwalletd to check
            tracing::info!(?test_type, "waiting for zebrad to sync to the tip");
            zebrad.expect_stdout_line_matches(SYNC_FINISHED_REGEX)?;

            // Wait for the state to upgrade, if the upgrade is long.
            // If this line hangs, change DATABASE_FORMAT_UPGRADE_IS_LONG to false.
            if DATABASE_FORMAT_UPGRADE_IS_LONG {
                wait_for_state_version_upgrade(
                    &mut zebrad,
                    &state_version_message,
                    state_database_format_version_in_code(),
                    None,
                )?;
            }

            (zebrad, None)
        }
    } else {
        let lightwalletd = lightwalletd_and_port.map(|(lightwalletd, _port)| lightwalletd);

        // We don't have a cached state, so we don't do any tip checks for Zebra or lightwalletd
        (zebrad, lightwalletd)
    };

    tracing::info!(
        ?test_type,
        "cleaning up child processes and checking for errors",
    );

    // Cleanup both processes
    //
    // If the test fails here, see the [note on port conflict](#Note on port conflict)
    //
    // zcash/lightwalletd exits by itself, but
    // adityapk00/lightwalletd keeps on going, so it gets killed by the test harness.
    zebrad.kill(false)?;

    if let Some(mut lightwalletd) = lightwalletd {
        lightwalletd.kill(false)?;

        let lightwalletd_output = lightwalletd.wait_with_output()?.assert_failure()?;

        lightwalletd_output
            .assert_was_killed()
            .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;
    }

    let zebrad_output = zebrad.wait_with_output()?.assert_failure()?;

    zebrad_output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")?;

    Ok(())
}
