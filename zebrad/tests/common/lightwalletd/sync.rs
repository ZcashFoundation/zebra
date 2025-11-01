//! Lightwalletd sync functions.

use std::{
    net::SocketAddr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use tempfile::TempDir;

use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_test::prelude::*;

use crate::common::{launch::ZebradTestDirExt, test_type::TestType};

/// The amount of time we wait between each tip check.
pub const TIP_CHECK_RATE_LIMIT: Duration = Duration::from_secs(60);

/// Wait for lightwalletd to sync to Zebra's tip.
///
/// If `wait_for_zebrad_mempool` is `true`, wait for Zebra to activate its mempool.
/// If `wait_for_zebrad_tip` is `true`, also wait for Zebra to sync to the network chain tip.
#[tracing::instrument]
pub fn wait_for_zebrad_and_lightwalletd_sync<
    P: ZebradTestDirExt + std::fmt::Debug + std::marker::Send + 'static,
>(
    mut lightwalletd: TestChild<TempDir>,
    lightwalletd_rpc_port: u16,
    mut zebrad: TestChild<P>,
    zebra_rpc_address: SocketAddr,
    test_type: TestType,
    wait_for_zebrad_mempool: bool,
    wait_for_zebrad_tip: bool,
) -> Result<(TestChild<TempDir>, TestChild<P>)> {
    let is_zebrad_finished = AtomicBool::new(!wait_for_zebrad_tip);
    let is_lightwalletd_finished = AtomicBool::new(false);

    let is_zebrad_finished = &is_zebrad_finished;
    let is_lightwalletd_finished = &is_lightwalletd_finished;

    // TODO: split these closures out into their own functions

    // Check Zebra's logs for errors.
    // Optionally waits until Zebra has synced to the tip, based on `wait_for_zebrad_tip`.
    //
    // `lightwalletd` syncs can take a long time,
    // so we need to check that `zebrad` has synced to the tip in parallel.
    let zebrad_mut = &mut zebrad;
    let zebrad_wait_fn = || -> Result<_> {
        // When we are near the tip, the mempool should activate at least once
        if wait_for_zebrad_mempool {
            tracing::info!(
                ?test_type,
                "waiting for zebrad to activate the mempool when it gets near the tip..."
            );
            zebrad_mut.expect_stdout_line_matches("activating mempool")?;
        }

        // When we are near the tip, this message is logged multiple times
        if wait_for_zebrad_tip {
            tracing::info!(?test_type, "waiting for zebrad to sync to the tip...");
            zebrad_mut.expect_stdout_line_matches(crate::common::sync::SYNC_FINISHED_REGEX)?;
        }

        // Tell the other thread that Zebra has finished
        is_zebrad_finished.store(true, Ordering::SeqCst);

        tracing::info!(
            ?test_type,
            "zebrad is waiting for lightwalletd to sync to the tip..."
        );
        while !is_lightwalletd_finished.load(Ordering::SeqCst) {
            // Just keep checking the Zebra logs for errors...
            if wait_for_zebrad_tip {
                // Make sure the sync is still finished, this is logged every minute or so.
                zebrad_mut.expect_stdout_line_matches(crate::common::sync::SYNC_FINISHED_REGEX)?;
            } else {
                // Use sync progress logs, which are logged every minute or so.
                zebrad_mut.expect_stdout_line_matches(crate::common::sync::SYNC_PROGRESS_REGEX)?;
            }
        }

        Ok(zebrad_mut)
    };

    // Wait until `lightwalletd` has synced to Zebra's tip.
    // Calls `lightwalletd`'s gRPCs and Zebra's JSON-RPCs.
    // Also checks `lightwalletd`'s logs for errors.
    //
    // `zebrad` syncs can take a long time,
    // so we need to check that `lightwalletd` has synced to the tip in parallel.
    let lightwalletd_mut = &mut lightwalletd;
    let lightwalletd_wait_fn = || -> Result<_> {
        tracing::info!(
            ?test_type,
            "lightwalletd is waiting for zebrad to sync to the tip..."
        );
        while !is_zebrad_finished.load(Ordering::SeqCst) {
            // Just keep checking the `lightwalletd` logs for errors.
            // It usually logs something every 30-90 seconds,
            // but there's no specific message we need to wait for here.
            assert!(
                lightwalletd_mut.wait_for_stdout_line(None),
                "lightwalletd output unexpectedly finished early",
            );
        }

        tracing::info!(?test_type, "waiting for lightwalletd to sync to the tip...");
        while !are_zebrad_and_lightwalletd_tips_synced(zebra_rpc_address, lightwalletd_mut)? {
            let previous_check = Instant::now();

            // To improve performance, only check the tips occasionally
            while previous_check.elapsed() < TIP_CHECK_RATE_LIMIT {
                assert!(
                    lightwalletd_mut.wait_for_stdout_line(None),
                    "lightwalletd output unexpectedly finished early",
                );
            }
        }

        // Tell the other thread that `lightwalletd` has finished
        is_lightwalletd_finished.store(true, Ordering::SeqCst);

        Ok(lightwalletd_mut)
    };

    // Run both threads in parallel, automatically propagating any panics to this thread.
    std::thread::scope(|s| {
        // Launch the sync-waiting threads
        let zebrad_thread = s.spawn(|| {
            let zebrad_result = zebrad_wait_fn();
            is_zebrad_finished.store(true, Ordering::SeqCst);

            zebrad_result.expect("test failed while waiting for zebrad to sync");
        });

        let lightwalletd_thread = s.spawn(|| {
            let lightwalletd_result = lightwalletd_wait_fn();
            is_lightwalletd_finished.store(true, Ordering::SeqCst);

            lightwalletd_result.expect("test failed while waiting for lightwalletd to sync.");
        });

        // Mark the sync-waiting threads as finished if they fail or panic.
        // This tells the other thread that it can exit.
        //
        // TODO: use `panic::catch_unwind()` instead,
        //       when `&mut zebra_test::command::TestChild<TempDir>` is unwind-safe
        s.spawn(|| {
            let zebrad_result = zebrad_thread.join();
            is_zebrad_finished.store(true, Ordering::SeqCst);

            zebrad_result.expect("test panicked or failed while waiting for zebrad to sync");
        });
        s.spawn(|| {
            let lightwalletd_result = lightwalletd_thread.join();
            is_lightwalletd_finished.store(true, Ordering::SeqCst);

            lightwalletd_result
                .expect("test panicked or failed while waiting for lightwalletd to sync");
        });
    });

    Ok((lightwalletd, zebrad))
}

/// Returns `Ok(true)` if zebrad and lightwalletd are both at the same height.
#[tracing::instrument]
pub fn are_zebrad_and_lightwalletd_tips_synced(
    zebra_rpc_address: SocketAddr,
    lightwalletd: &mut TestChild<TempDir>,
) -> Result<bool> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        // We are going to try getting the current lightwalletd height by reading the lightwalletd logs.
        let mut lightwalletd_next_height = 1;

        // Only go forward on getting next height from lightwalletd logs if we find the line we are interested in.
        //
        // TODO: move this blocking code out of the async executor.
        // The executor could block all tasks and futures while this code is running.
        // That's ok for now, but it might cause test hangs or failures if we spawn tasks, select(), or join().
        if let Ok(line) = lightwalletd.expect_stdout_line_matches("Waiting for block: [0-9]+") {
            let line_json: serde_json::Value = serde_json::from_str(line.as_str())
                .expect("captured lightwalletd logs are always valid json");
            let msg = line_json["msg"]
                .as_str()
                .expect("`msg` field is always a valid string");

            // Block number is the last word of the message. We rely on that specific for this to work.
            let last = msg
                .split(' ')
                .next_back()
                .expect("always possible to get the last word of a separated by space string");
            lightwalletd_next_height = last
                .parse()
                .expect("the last word is always the block number so it can be parsed to i32 ");
        }

        // The last height in lightwalletd is the one the program is expecting minus one.
        let lightwalletd_tip_height = (lightwalletd_next_height - 1) as u64;

        // Get the block tip from zebrad
        let client = RpcRequestClient::new(zebra_rpc_address);
        let zebrad_blockchain_info = client
            .text_from_call("getblockchaininfo", "[]".to_string())
            .await?;
        let zebrad_blockchain_info: serde_json::Value =
            serde_json::from_str(&zebrad_blockchain_info)?;
        let zebrad_tip_height = zebrad_blockchain_info["result"]["blocks"]
            .as_u64()
            .expect("unexpected block height: doesn't fit in u64");

        if lightwalletd_tip_height != zebrad_tip_height {
            tracing::info!(
                lightwalletd_tip_height,
                zebrad_tip_height,
                zebra_rpc_address = ?zebra_rpc_address,
                "lightwalletd tip is behind Zebra tip, waiting for sync",
            );
        } else {
            tracing::debug!(
                lightwalletd_tip_height,
                zebrad_tip_height,
                zebra_rpc_address = ?zebra_rpc_address,
                "lightwalletd tip matches Zebra tip",
            );
        }

        Ok(lightwalletd_tip_height == zebrad_tip_height)
    })
}
