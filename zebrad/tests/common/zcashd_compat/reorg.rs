//! Reorg regression and stress test bodies for the zcashd-compat integration suite.

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use color_eyre::eyre::{eyre, Result};
use tokio::time::sleep;

use super::{
    launch::ZcashdCompatSetup, setup_zcashd_compat, ZcashdRpcClient,
    TEST_ZCASHD_COMPAT_REORG_ITERATIONS,
};
use crate::common::regtest::MiningRpcMethods;

const SYNC_BATCH_SIZE_LIMIT: u64 = 33;
const DEFAULT_REORG_CHURN_ITERATIONS: u32 = 30;
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

fn zcashd_pid(setup: &ZcashdCompatSetup) -> Result<u32> {
    let datadir = setup
        .zcashd_datadir
        .as_ref()
        .ok_or_else(|| eyre!("zcashd datadir is unavailable outside managed regtest mode"))?;
    let pid_path: PathBuf = datadir.join("regtest").join("zcashd.pid");
    let pid = std::fs::read_to_string(&pid_path)
        .map_err(|e| eyre!("failed to read zcashd pid file {}: {e}", pid_path.display()))?;

    pid.trim()
        .parse()
        .map_err(|e| eyre!("invalid zcashd pid in {}: {e}", pid_path.display()))
}

struct ZcashdPauseGuard {
    pid: u32,
    paused: bool,
}

impl ZcashdPauseGuard {
    fn pause(setup: &ZcashdCompatSetup) -> Result<Self> {
        let pid = zcashd_pid(setup)?;
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

fn send_signal(pid: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .map_err(|e| eyre!("failed to run kill {signal} {pid}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(eyre!("kill {signal} {pid} failed with status {status}"))
    }
}

/// Forces a Zebra-side reorg while zcashd is paused so it observes the new best chain atomically.
///
/// Do not use unpaused reorgs deeper than 1: zcashd can otherwise observe the
/// intermediate shorter chain and enter a sticky local-tip-ahead failure.
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
