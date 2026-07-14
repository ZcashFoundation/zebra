//! Process-lifecycle test bodies for the zcashd-compat integration test suite.

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tokio::time::sleep;

use super::setup_zcashd_compat;

/// Verifies that zebrad shuts down cleanly while supervising a running zcashd.
///
/// Only runs in managed (regtest) mode; skipped on external networks where we
/// do not own the zebrad process.
pub async fn zebrad_clean_shutdown() -> Result<()> {
    let Some(mut setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    let mut zebrad = setup
        .managed
        .take()
        .expect("managed process is present in regtest mode");

    zebrad.kill(false)?;
    zebrad
        .wait_with_output()?
        .assert_failure()?
        .assert_was_killed()?;

    Ok(())
}

/// Verifies that zcashd restarts automatically after an unexpected exit while
/// zebrad's supervisor is running.
///
/// Triggers a clean zcashd shutdown via its own `stop` RPC, waits for the
/// supervisor to restart it, then verifies zcashd is responsive again.
///
/// Only runs in managed (regtest) mode.
pub async fn zcashd_restarts_after_exit() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    // Ask zcashd to stop gracefully; the zebrad supervisor should restart it.
    let _: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("stop", "[]")
        .await
        .map_err(|e| eyre!("zcashd stop: {e}"))?;

    // Wait for zcashd to exit and the supervisor to restart it (up to 30 s).
    let mut recovered = false;
    for attempt in 1..=30u32 {
        sleep(Duration::from_secs(1)).await;
        let result = setup
            .zcashd_client
            .json_result_from_call::<serde_json::Value>("getblockchaininfo", "[]")
            .await;
        if result.is_ok() {
            recovered = true;
            break;
        }
        if attempt == 30 {
            break;
        }
    }

    assert!(
        recovered,
        "zcashd did not come back up within 30 s after stop"
    );

    setup.teardown()
}
