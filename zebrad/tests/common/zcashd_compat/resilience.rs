//! Process-lifecycle test bodies for the zcashd-compat integration test suite.

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tokio::time::sleep;

use super::{launch::send_signal, setup_zcashd_compat};

/// Verifies that an abruptly SIGKILLed zebrad exits while supervising a
/// running zcashd, and that the test harness cleans up the orphaned sidecar.
///
/// SIGKILL cannot be handled, so this deliberately skips every zebrad
/// shutdown path; graceful shutdown is covered by
/// [`zebrad_graceful_shutdown_stops_zcashd`].
///
/// Only runs in managed (regtest) mode; skipped on external networks where we
/// do not own the zebrad process.
pub async fn zebrad_abrupt_kill() -> Result<()> {
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

    // `setup` is dropped here: its `Drop` impl kills the orphaned zcashd.
    Ok(())
}

/// Verifies that zebrad's graceful shutdown (SIGTERM) also stops the
/// supervised zcashd: zebrad's post-runtime cleanup SIGTERMs the child and
/// waits for it, so a service-manager stop cannot orphan the sidecar.
///
/// Only runs in managed (regtest) mode.
#[cfg(unix)]
pub async fn zebrad_graceful_shutdown_stops_zcashd() -> Result<()> {
    let Some(mut setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    let zcashd_pid = setup.zcashd_pid()?;

    let zebrad = setup
        .managed
        .take()
        .expect("managed process is present in regtest mode");
    let zebrad_pid = zebrad
        .child
        .as_ref()
        .expect("zebrad has not been waited on yet")
        .id();

    send_signal(zebrad_pid, "-TERM")?;

    // zebrad exits, then its post-runtime cleanup terminates zcashd.
    zebrad.wait_with_output()?;

    let mut zcashd_exited = false;
    for _ in 0..60u32 {
        // `kill -0` only checks whether the process still exists.
        if send_signal(zcashd_pid, "-0").is_err() {
            zcashd_exited = true;
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }

    assert!(
        zcashd_exited,
        "supervised zcashd (pid {zcashd_pid}) should exit within 60 s of zebrad's SIGTERM"
    );

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
