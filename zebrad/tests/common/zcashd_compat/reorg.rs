//! Reorg regression and stress test bodies for the zcashd-compat integration suite.

use std::time::{Duration, Instant};

use color_eyre::eyre::{eyre, Result};
use tokio::time::sleep;

use super::{
    launch::{send_signal, ZcashdCompatSetup},
    setup_zcashd_compat, ZcashdRpcClient, TEST_ZCASHD_COMPAT_REORG_ITERATIONS,
    TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG,
};
use crate::common::regtest::MiningRpcMethods;

const SYNC_BATCH_SIZE_LIMIT: u64 = 33;
const DEFAULT_REORG_CHURN_ITERATIONS: u32 = 30;
const CHAIN_HEIGHT_DEEP: u32 = 295;
const STANDARD_SYNC_TIMEOUT: Duration = Duration::from_secs(90);
const DEEP_REORG_SYNC_TIMEOUT: Duration = Duration::from_secs(120);

/// Verifies that zcashd follows a basic Zebra depth-1 reorg.
pub async fn basic_depth1() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    setup.zebra_client.generate(10).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    force_zebra_reorg(&setup, 9, 2).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    let info = compat_info(&setup.zcashd_client).await?;
    assert_eq!(info["sync"]["state"].as_str(), Some("synced"));
    assert_eq!(info["sync"]["detail"].as_str(), Some("zebra_tip_matched"));

    setup.teardown()
}

/// Regression test for same-height equal-work reorgs that zcashd cannot activate immediately.
pub async fn equal_work_race() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    setup.zebra_client.generate(10).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    let old_zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;

    force_zebra_reorg(&setup, 9, 1).await?;

    let info = wait_for_sync_detail(
        &setup.zcashd_client,
        "zebra_equal_work_reorg_not_activated",
        STANDARD_SYNC_TIMEOUT,
    )
    .await?;
    let zebra_tip = zebra_tip(&setup).await?;
    let zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;

    assert_eq!(info["sync"]["state"].as_str(), Some("degraded"));
    assert_eq!(info["readiness"].as_str(), Some("degraded"));
    assert_eq!(
        zcashd_tip, old_zcashd_tip,
        "zcashd should keep the first-seen equal-work tip until Zebra extends"
    );
    assert_ne!(
        zcashd_tip.1, zebra_tip.1,
        "the equal-work race requires same-height competing tips"
    );

    setup.zebra_client.generate(1).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;
    wait_for_readiness(&setup.zcashd_client, "ready", STANDARD_SYNC_TIMEOUT).await?;

    setup.teardown()
}

/// Verifies that zcashd can ingest a replacement branch at its batch-size limit.
pub async fn depth_at_batch_limit() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    setup.zebra_client.generate(40).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    let info = compat_info(&setup.zcashd_client).await?;
    assert_eq!(
        info["limits"]["sync_batch_size"].as_u64(),
        Some(SYNC_BATCH_SIZE_LIMIT),
        "test assumes zcashd's memory-clamped sync batch limit is 33"
    );

    force_zebra_reorg(&setup, 8, SYNC_BATCH_SIZE_LIMIT as u32).await?;
    wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT).await?;

    setup.teardown()
}

/// Verifies that replacement branches larger than zcashd's batch limit fail sticky.
pub async fn branch_too_large_sticky() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    setup.zebra_client.generate(40).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    let old_zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;

    force_zebra_reorg(&setup, 8, (SYNC_BATCH_SIZE_LIMIT + 1) as u32).await?;

    let first_failure = wait_for_sync_detail(
        &setup.zcashd_client,
        "reorg_branch_too_large",
        STANDARD_SYNC_TIMEOUT,
    )
    .await?;
    assert_eq!(first_failure["sync"]["state"].as_str(), Some("failed"));
    assert_eq!(first_failure["readiness"].as_str(), Some("failed"));

    sleep(Duration::from_secs(4)).await;

    let second_failure = compat_info(&setup.zcashd_client).await?;
    assert_eq!(
        second_failure["sync"]["detail"].as_str(),
        Some("reorg_branch_too_large"),
        "oversized replacement branch should stay in the same sticky failure"
    );
    assert_eq!(second_failure["sync"]["state"].as_str(), Some("failed"));
    assert_eq!(second_failure["readiness"].as_str(), Some("failed"));

    let current_zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;
    assert_eq!(
        current_zcashd_tip, old_zcashd_tip,
        "zcashd should not advance after rejecting an oversized replacement branch"
    );

    setup.teardown()
}

/// Sticky sync fault clears after restart once Zebra's best chain is reconciled.
///
/// `UnityBlockSourceThread` idles once `stickyFault` is set. Reconcile Zebra first, then
/// restart zcashd so the sync loop retries against an ingestible branch instead of
/// immediately re-entering sticky failure on the oversized replacement chain.
pub async fn sticky_fault_restart_recovers() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    const FORK_HEIGHT: u64 = 8;

    setup.zebra_client.generate(40).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    let old_zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;

    force_zebra_reorg(&setup, FORK_HEIGHT, (SYNC_BATCH_SIZE_LIMIT + 1) as u32).await?;

    let first_failure = wait_for_sync_detail(
        &setup.zcashd_client,
        "reorg_branch_too_large",
        STANDARD_SYNC_TIMEOUT,
    )
    .await?;
    assert_eq!(first_failure["sync"]["state"].as_str(), Some("failed"));
    assert_eq!(first_failure["readiness"].as_str(), Some("failed"));

    sleep(Duration::from_secs(4)).await;

    let still_failed = compat_info(&setup.zcashd_client).await?;
    assert_eq!(
        still_failed["sync"]["detail"].as_str(),
        Some("reorg_branch_too_large"),
        "oversized branch should remain sticky before restart"
    );
    assert_eq!(still_failed["sync"]["state"].as_str(), Some("failed"));

    let current_zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;
    assert_eq!(
        current_zcashd_tip,
        old_zcashd_tip,
        "zcashd should keep the pre-reorg tip while sticky"
    );

    let replacement_root = zebra_block_hash(&setup, FORK_HEIGHT + 1).await?;
    let params = serde_json::to_string(&vec![replacement_root])?;
    let _: () = setup
        .zebra_client
        .json_result_from_call("invalidateblock", &params)
        .await
        .map_err(|e| eyre!("revert oversized Zebra branch: {e}"))?;

    // Zebra falls back to the common ancestor at `FORK_HEIGHT`. Mine one block past zcashd's
    // height so the replacement branch strictly beats on work and fits the batch limit.
    let zcashd_height = old_zcashd_tip.0;
    let extension_len = (zcashd_height - FORK_HEIGHT + 1) as u32;
    assert!(
        extension_len <= SYNC_BATCH_SIZE_LIMIT as u32,
        "replacement branch length {extension_len} exceeds batch limit"
    );
    setup
        .zebra_client
        .generate(extension_len)
        .await
        .map_err(|e| eyre!("extend Zebra past zcashd after revert: {e}"))?;

    let old_pid = setup.zcashd_pid()?;
    let _: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("stop", "[]")
        .await
        .map_err(|e| eyre!("zcashd stop after sticky fault: {e}"))?;
    wait_for_restarted_zcashd_rpc(&setup, old_pid, STANDARD_SYNC_TIMEOUT).await?;

    wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT).await?;
    wait_for_readiness(&setup.zcashd_client, "ready", STANDARD_SYNC_TIMEOUT).await?;

    let info = compat_info(&setup.zcashd_client).await?;
    assert_eq!(
        info["sync"]["detail"].as_str(),
        Some("zebra_tip_matched"),
        "zcashd should recover after sticky fault + restart + Zebra reconciliation"
    );

    setup.teardown()
}

/// Opt-in probe for zcashd supervisor restart after Zebra-side regtest reorgs.
///
/// Exercises VerifyDB and LoadBlockIndex with trusted Zebra regtest side-branch
/// block-index entries on disk after several reorgs. Opt-in because these restart
/// probes are slow, not because of a known crash.
#[allow(clippy::print_stderr)]
pub async fn restart_after_reorg() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    if std::env::var_os(TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG).is_none() {
        eprintln!(
            "Skipped restart-after-reorg reload probe; set \
             {TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG}=1 to run slow restart probes"
        );
        return setup.teardown();
    }

    setup.zebra_client.generate(12).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    for depth in 1..=3 {
        let fork_height = zebra_tip(&setup).await?.0 - u64::from(depth);
        force_zebra_reorg(&setup, fork_height, depth + 1)
            .await
            .map_err(|e| eyre!("force depth-{depth} reorg before restart: {e}"))?;
        wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT)
            .await
            .map_err(|e| eyre!("wait for depth-{depth} reorg convergence before restart: {e}"))?;
    }

    restart_zcashd_and_wait_for_tips(&setup).await?;

    setup.teardown()
}

/// Interleaved reorg-then-restart across three cycles.
///
/// Each restart boots from a chain that already survived one restart, so trusted-boundary
/// advancement on disk and VerifyDB / LoadBlockIndex keep working across cycles.
#[allow(clippy::print_stderr)]
pub async fn restart_cycles() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    if std::env::var_os(TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG).is_none() {
        eprintln!(
            "Skipped restart-cycles probe; set \
             {TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG}=1 to run slow restart probes"
        );
        return setup.teardown();
    }

    setup.zebra_client.generate(15).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    for cycle in 1u64..=3 {
        let tip_height = zebra_tip(&setup).await?.0;
        let fork_height = tip_height - cycle;
        force_zebra_reorg(&setup, fork_height, cycle as u32 + 2)
            .await
            .map_err(|e| eyre!("cycle {cycle}: force reorg: {e}"))?;
        wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT)
            .await
            .map_err(|e| eyre!("cycle {cycle}: wait for reorg convergence: {e}"))?;
        restart_zcashd_and_wait_for_tips(&setup)
            .await
            .map_err(|e| eyre!("cycle {cycle}: restart: {e}"))?;

        let info = compat_info(&setup.zcashd_client).await?;
        assert_eq!(
            info["sync"]["detail"].as_str(),
            Some("zebra_tip_matched"),
            "cycle {cycle}: sync loop not healthy after restart; detail: {:?}",
            info["sync"]["detail"]
        );
    }

    setup.teardown()
}

/// VerifyDB window coverage on a long trusted chain after reorg and restart.
///
/// When the active chain exceeds zcashd's default `-checkblocks=288` window, trusted Zebra
/// regtest blocks must skip PoW checks in VerifyDB and LoadBlockIndex must not fail on
/// disconnected side-branch entries below the trusted boundary.
#[allow(clippy::print_stderr)]
pub async fn restart_deep_chain() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    if std::env::var_os(TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG).is_none() {
        eprintln!(
            "Skipped restart-deep-chain probe; set \
             {TEST_ZCASHD_COMPAT_RESTART_AFTER_REORG}=1 to run slow restart probes"
        );
        return setup.teardown();
    }

    setup.zebra_client.generate(CHAIN_HEIGHT_DEEP).await?;
    wait_for_tips_match(&setup, Duration::from_secs(240)).await?;

    force_zebra_reorg(&setup, (CHAIN_HEIGHT_DEEP - 10) as u64, 12).await?;
    wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT).await?;
    restart_zcashd_and_wait_for_tips(&setup).await?;

    let info = compat_info(&setup.zcashd_client).await?;
    assert_eq!(
        info["sync"]["detail"].as_str(),
        Some("zebra_tip_matched"),
        "sync loop not healthy after deep-chain restart; detail: {:?}",
        info["sync"]["detail"]
    );

    setup.teardown()
}

/// Requires zcashd to recover when Zebra's best tip temporarily rolls behind zcashd's local tip.
///
/// zcashd must report `zebra_tip_behind_local` as a degraded, retryable state,
/// then recover when Zebra mines a replacement branch. Sticky local-tip-ahead
/// failures are regressions.
pub async fn zebra_tip_behind_local() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    setup.zebra_client.generate(10).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    let old_zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;
    let old_zebra_tip = zebra_tip(&setup).await?;
    assert_eq!(old_zcashd_tip, old_zebra_tip);

    let params = serde_json::to_string(&vec![old_zebra_tip.1])?;
    let _: () = setup
        .zebra_client
        .json_result_from_call("invalidateblock", &params)
        .await
        .map_err(|e| eyre!("zebrad invalidate tip block: {e}"))?;

    let info = wait_for_sync_detail(
        &setup.zcashd_client,
        "zebra_tip_behind_local",
        STANDARD_SYNC_TIMEOUT,
    )
    .await?;

    assert_eq!(info["sync"]["state"].as_str(), Some("degraded"));
    assert_eq!(info["readiness"].as_str(), Some("degraded"));

    setup.zebra_client.generate(2).await?;
    wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT).await?;
    wait_for_readiness(&setup.zcashd_client, "ready", STANDARD_SYNC_TIMEOUT).await?;

    setup.teardown()
}

/// No sticky failure when Zebra shrinks after a paused reorg has fully converged.
///
/// After zcashd completes reorg ingestion, an unpaused tip invalidation exercises
/// tip-behind recovery without sticky `failed` sync state. The `reorgContext` transient
/// (`zebra_tip_temporarily_behind_local_after_reorg`) is covered by zcashd unit tests;
/// this integration test validates end-to-end recovery and asserts `zebra_tip_matched`.
pub async fn reorg_context_zebra_tip_behind_recovers() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    setup.zebra_client.generate(20).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    for round in 1u32..=3 {
        let tip_height = zebra_tip(&setup).await?.0;
        force_zebra_reorg(&setup, tip_height - 2, 3)
            .await
            .map_err(|e| eyre!("round {round}: force paused depth-2 reorg: {e}"))?;
        wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT)
            .await
            .map_err(|e| eyre!("round {round}: wait after reorg: {e}"))?;

        let new_tip_hash = zebra_tip(&setup).await?.1;
        let params = serde_json::to_string(&vec![new_tip_hash])?;
        let _: () = setup
            .zebra_client
            .json_result_from_call("invalidateblock", &params)
            .await
            .map_err(|e| eyre!("round {round}: invalidate new tip: {e}"))?;

        setup.zebra_client.generate(2).await?;
        wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT)
            .await
            .map_err(|e| eyre!("round {round}: {e}"))?;

        let info = compat_info(&setup.zcashd_client).await?;
        assert_ne!(
            info["sync"]["state"].as_str(),
            Some("failed"),
            "round {round}: zcashd entered sticky failure; detail: {:?}",
            info["sync"]["detail"]
        );
        assert_eq!(
            info["sync"]["detail"].as_str(),
            Some("zebra_tip_matched"),
            "round {round}: zcashd not synced after recovery"
        );
    }

    setup.teardown()
}

/// Repeatedly forces small reorgs and occasional mid-sync depth-1 churn.
pub async fn churn() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    setup.zebra_client.generate(12).await?;
    wait_for_tips_match(&setup, STANDARD_SYNC_TIMEOUT).await?;

    for cycle in 1..=reorg_churn_iterations()? {
        if cycle % 8 == 0 {
            setup
                .zebra_client
                .generate(30)
                .await
                .map_err(|e| eyre!("cycle {cycle}: generate burst before depth-1 reorg: {e}"))?;
            force_unpaused_depth1_reorg(&setup)
                .await
                .map_err(|e| eyre!("cycle {cycle}: force unpaused depth-1 reorg: {e}"))?;
        } else {
            let depth = (cycle % 3) + 1;
            let fork_height = zebra_tip(&setup).await?.0 - u64::from(depth);
            force_zebra_reorg(&setup, fork_height, depth + 1)
                .await
                .map_err(|e| eyre!("cycle {cycle}: force depth-{depth} reorg: {e}"))?;
        }

        wait_for_tips_match(&setup, DEEP_REORG_SYNC_TIMEOUT)
            .await
            .map_err(|e| eyre!("cycle {cycle}: {e}"))?;
    }

    setup.teardown()
}

async fn zebra_tip(setup: &ZcashdCompatSetup) -> Result<(u64, String)> {
    let info: serde_json::Value = setup
        .zebra_client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|e| eyre!("zebrad getblockchaininfo: {e}"))?;

    tip_from_blockchain_info("zebrad", info)
}

async fn zebra_block_hash(setup: &ZcashdCompatSetup, height: u64) -> Result<String> {
    setup
        .zebra_client
        .json_result_from_call("getblockhash", format!("[{height}]"))
        .await
        .map_err(|e| eyre!("zebrad getblockhash({height}): {e}"))
}

async fn zcashd_tip(client: &ZcashdRpcClient) -> Result<(u64, String)> {
    let info: serde_json::Value = client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|e| eyre!("zcashd getblockchaininfo: {e}"))?;

    tip_from_blockchain_info("zcashd", info)
}

fn tip_from_blockchain_info(node: &str, info: serde_json::Value) -> Result<(u64, String)> {
    let height = info["blocks"]
        .as_u64()
        .ok_or_else(|| eyre!("{node} getblockchaininfo missing numeric blocks: {info}"))?;
    let hash = info["bestblockhash"]
        .as_str()
        .ok_or_else(|| eyre!("{node} getblockchaininfo missing string bestblockhash: {info}"))?
        .to_string();

    Ok((height, hash))
}

async fn compat_info(client: &ZcashdRpcClient) -> Result<serde_json::Value> {
    client
        .json_result_from_call("getzebracompatinfo", "[]")
        .await
        .map_err(|e| eyre!("getzebracompatinfo: {e}"))
}

struct ZcashdPauseGuard {
    pid: u32,
    paused: bool,
}

impl ZcashdPauseGuard {
    fn pause(setup: &ZcashdCompatSetup) -> Result<Self> {
        let pid = setup.zcashd_pid()?;
        send_signal(pid, "-STOP")?;

        Ok(Self { pid, paused: true })
    }

    fn resume(&mut self) -> Result<()> {
        if self.paused {
            send_signal(self.pid, "-CONT")?;
            self.paused = false;
        }

        Ok(())
    }
}

impl Drop for ZcashdPauseGuard {
    fn drop(&mut self) {
        if self.paused {
            let _ = send_signal(self.pid, "-CONT");
            self.paused = false;
        }
    }
}

async fn restart_zcashd_and_wait_for_tips(setup: &ZcashdCompatSetup) -> Result<()> {
    let old_pid = setup.zcashd_pid()?;

    let _: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("stop", "[]")
        .await
        .map_err(|e| eyre!("zcashd stop: {e}"))?;

    wait_for_restarted_zcashd_rpc(setup, old_pid, STANDARD_SYNC_TIMEOUT).await?;
    wait_for_readiness(&setup.zcashd_client, "ready", STANDARD_SYNC_TIMEOUT).await?;
    wait_for_tips_match(setup, STANDARD_SYNC_TIMEOUT).await
}

async fn wait_for_restarted_zcashd_rpc(
    setup: &ZcashdCompatSetup,
    old_pid: u32,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;

    loop {
        sleep(Duration::from_secs(1)).await;

        let rpc_result = setup
            .zcashd_client
            .json_result_from_call::<serde_json::Value>("getblockchaininfo", "[]")
            .await;

        let last_seen = match rpc_result {
            Ok(_) => match setup.zcashd_pid() {
                Ok(new_pid) if new_pid != old_pid => return Ok(()),
                Ok(new_pid) => format!("zcashd RPC responded from original pid {new_pid}"),
                Err(error) => format!("zcashd RPC responded but pid was unavailable: {error}"),
            },
            Err(error) => format!("zcashd RPC unavailable: {error}"),
        };

        if Instant::now() >= deadline {
            return Err(eyre!(
                "zcashd did not restart within {timeout:?}; last seen: {last_seen}"
            ));
        }
    }
}

/// Forces a Zebra-side reorg while zcashd is paused so it observes the new best chain atomically.
///
/// Paused reorgs avoid observable intermediate shorter-chain states during test
/// orchestration. Unpaused depth >1 reorgs can leave zcashd in a transient retryable
/// degraded state until Zebra's replacement branch extends.
async fn force_zebra_reorg(
    setup: &ZcashdCompatSetup,
    fork_height: u64,
    new_branch_len: u32,
) -> Result<()> {
    let invalidated_hash = zebra_block_hash(setup, fork_height + 1).await?;
    let params = serde_json::to_string(&vec![invalidated_hash])?;
    let mut pause_guard = ZcashdPauseGuard::pause(setup)?;

    let _: () = setup
        .zebra_client
        .json_result_from_call("invalidateblock", &params)
        .await
        .map_err(|e| eyre!("zebrad invalidateblock: {e}"))?;
    setup.zebra_client.generate(new_branch_len).await?;

    pause_guard.resume()
}

async fn force_unpaused_depth1_reorg(setup: &ZcashdCompatSetup) -> Result<()> {
    let tip_hash = zebra_tip(setup).await?.1;
    let params = serde_json::to_string(&vec![tip_hash])?;

    let _: () = setup
        .zebra_client
        .json_result_from_call("invalidateblock", &params)
        .await
        .map_err(|e| eyre!("zebrad invalidateblock: {e}"))?;
    setup.zebra_client.generate(2).await?;

    Ok(())
}

async fn wait_for_tips_match(setup: &ZcashdCompatSetup, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last_seen;

    loop {
        let zebra_tip = zebra_tip(setup).await?;
        let zcashd_tip = zcashd_tip(&setup.zcashd_client).await?;

        if zebra_tip == zcashd_tip {
            return Ok(());
        }

        last_seen = Some((zebra_tip, zcashd_tip));

        if Instant::now() >= deadline {
            return Err(eyre!(
                "tips did not match within {timeout:?}; last seen: {last_seen:?}"
            ));
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_sync_detail(
    client: &ZcashdRpcClient,
    expected: &str,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    let mut last_seen;

    loop {
        let info = compat_info(client).await?;
        let detail = info["sync"]["detail"].as_str().map(str::to_string);

        if detail.as_deref() == Some(expected) {
            return Ok(info);
        }

        last_seen = Some(info);

        if Instant::now() >= deadline {
            return Err(eyre!(
                "sync.detail did not become {expected:?} within {timeout:?}; last seen: {last_seen:?}"
            ));
        }

        sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_readiness(
    client: &ZcashdRpcClient,
    expected: &str,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    let mut last_seen;

    loop {
        let info = compat_info(client).await?;
        let readiness = info["readiness"].as_str().map(str::to_string);

        if readiness.as_deref() == Some(expected) {
            return Ok(info);
        }

        last_seen = Some(info);

        if Instant::now() >= deadline {
            return Err(eyre!(
                "readiness did not become {expected:?} within {timeout:?}; last seen: {last_seen:?}"
            ));
        }

        sleep(Duration::from_secs(1)).await;
    }
}

fn reorg_churn_iterations() -> Result<u32> {
    match std::env::var(TEST_ZCASHD_COMPAT_REORG_ITERATIONS) {
        Ok(value) if !value.is_empty() => value.parse().map_err(|e| {
            eyre!("invalid {TEST_ZCASHD_COMPAT_REORG_ITERATIONS} value {value:?}: {e}")
        }),
        _ => Ok(DEFAULT_REORG_CHURN_ITERATIONS),
    }
}
