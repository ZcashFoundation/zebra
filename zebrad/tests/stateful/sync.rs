use color_eyre::eyre::{Result, WrapErr};

use zebra_chain::{
    block,
    parameters::Network::{self, *},
};
use zebra_state::state_database_format_version_in_code;
use zebra_test::prelude::*;

use crate::common::{
    cached_state::{
        wait_for_state_version_message, wait_for_state_version_upgrade,
        DATABASE_FORMAT_UPGRADE_IS_LONG,
    },
    launch::spawn_zebrad_for_rpc,
    lightwalletd::{can_spawn_lightwalletd_for_rpc, spawn_lightwalletd_for_rpc},
    sync::{create_cached_database_height, STOP_AT_HEIGHT_REGEX, SYNC_FINISHED_REGEX},
    test_type::TestType::{self, *},
};

#[tracing::instrument]
fn create_cached_database(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height();
    let checkpoint_stop_regex =
        format!("{STOP_AT_HEIGHT_REGEX}.*commit checkpoint-verified request");

    create_cached_database_height(
        &network,
        height,
        // Use checkpoints to increase sync performance while caching the database
        true,
        // Check that we're still using checkpoints when we finish the cached sync
        &checkpoint_stop_regex,
    )
}

#[tracing::instrument]
fn sync_past_mandatory_checkpoint(network: Network) -> Result<()> {
    let height = network.mandatory_checkpoint_height() + (32_257 + 1200);
    let full_validation_stop_regex =
        format!("{STOP_AT_HEIGHT_REGEX}.*commit contextually-verified request");

    create_cached_database_height(
        &network,
        height.unwrap(),
        // Test full validation by turning checkpoints off
        false,
        // Check that we're doing full validation when we finish the cached sync
        &full_validation_stop_regex,
    )
}

/// Sync `network` until the chain tip is reached, or a timeout elapses.
///
/// The timeout is specified using an environment variable, with the name configured by the
/// `timeout_argument_name` parameter. The value of the environment variable must the number of
/// minutes specified as an integer.
#[allow(clippy::print_stderr)]
#[tracing::instrument]
fn full_sync_test(network: Network, timeout_argument_name: &str) -> Result<()> {
    // # TODO
    //
    // Replace hard-coded values in create_cached_database_height with:
    // - the timeout in the environmental variable
    // - the path from the resolved config (state.cache_dir)
    create_cached_database_height(
        &network,
        // Just keep going until we reach the chain tip
        block::Height::MAX,
        // Use the checkpoints to sync quickly, then do full validation until the chain tip
        true,
        // Finish when we reach the chain tip
        SYNC_FINISHED_REGEX,
    )
}

/// Sync up to the mandatory checkpoint height on mainnet and stop.
#[test]
#[ignore]
fn sync_to_mandatory_checkpoint_mainnet() -> Result<()> {
    sync_to_mandatory_checkpoint_for_network(Mainnet)
}

/// Sync to the mandatory checkpoint height testnet and stop.
#[test]
#[ignore]
fn sync_to_mandatory_checkpoint_testnet() -> Result<()> {
    sync_to_mandatory_checkpoint_for_network(Network::new_default_testnet())
}

/// Helper function for sync to checkpoint tests
fn sync_to_mandatory_checkpoint_for_network(network: Network) -> Result<()> {
    let _init_guard = zebra_test::init();
    create_cached_database(network)
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on mainnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on mainnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[test]
fn sync_past_mandatory_checkpoint_mainnet() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Mainnet;
    sync_past_mandatory_checkpoint(network)
}

/// Test syncing 1200 blocks (3 checkpoints) past the mandatory checkpoint on testnet.
///
/// This assumes that the config'd state is already synced at or near the mandatory checkpoint
/// activation on testnet. If the state has already synced past the mandatory checkpoint
/// activation by 1200 blocks, it will fail.
#[allow(dead_code)]
#[test]
fn sync_past_mandatory_checkpoint_testnet() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    sync_past_mandatory_checkpoint(network)
}

/// Test if `zebrad` can fully sync the chain on mainnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `SYNC_FULL_MAINNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn sync_full_mainnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(Mainnet, "SYNC_FULL_MAINNET_TIMEOUT_MINUTES")
}

/// Test if `zebrad` can fully sync the chain on testnet.
///
/// This test takes a long time to run, so we don't run it by default. This test is only executed
/// if there is an environment variable named `SYNC_FULL_TESTNET_TIMEOUT_MINUTES` set with the number
/// of minutes to wait for synchronization to complete before considering that the test failed.
#[test]
#[ignore]
fn sync_full_testnet() -> Result<()> {
    // TODO: add "ZEBRA" at the start of this env var, to avoid clashes
    full_sync_test(
        Network::new_default_testnet(),
        "SYNC_FULL_TESTNET_TIMEOUT_MINUTES",
    )
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

/// Run a lightwalletd integration test with a configuration for `test_type`.
///
/// Tests that sync `lightwalletd` to the chain tip require the `lightwalletd-grpc-tests` feature`:
/// - [`FullSyncFromGenesis`]
/// - [`UpdateCachedState`]
///
/// Set `FullSyncFromGenesis { allow_lightwalletd_cached_state: true }` to speed up manual full sync tests.
///
/// # Reliability
///
/// The random ports in this test can cause [rare port conflicts.](#Note on port conflict)
///
/// # Panics
///
/// If the `test_type` requires `--features=lightwalletd-grpc-tests`,
/// but Zebra was not compiled with that feature.
#[tracing::instrument]
pub(super) fn lwd_integration_test(test_type: TestType) -> Result<()> {
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
            lightwalletd.expect_stdout_line_matches(regex::escape(
                "Got sapling height 419200 block height [0-9]{1,6} chain main branchID 00000000",
            ))?;
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
                use crate::common::lightwalletd::sync::wait_for_zebrad_and_lightwalletd_sync;

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
