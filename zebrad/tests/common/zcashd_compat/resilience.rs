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

    // Read the sidecar pid before killing zebrad: `wait_with_output()` consumes
    // the zebrad `TestChild` and deletes the testdir that holds the pid file,
    // which would disarm the `Drop` backstop, so the orphaned sidecar must be
    // killed here rather than relying on `Drop`.
    let zcashd_pid = setup.zcashd_pid().ok();

    let mut zebrad = setup
        .managed
        .take()
        .expect("managed process is present in regtest mode");

    zebrad.kill(false)?;

    // Kill the orphaned sidecar before consuming the testdir.
    if let Some(pid) = zcashd_pid {
        let _ = send_signal(pid, "-KILL");
    }

    zebrad
        .wait_with_output()?
        .assert_failure()?
        .assert_was_killed()?;

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

    // If the product bug is present, zcashd is still running here. Kill it
    // before the assert so a failing test never leaks the sidecar (zebrad's
    // testdir is already gone, so the Drop backstop cannot).
    if !zcashd_exited {
        let _ = send_signal(zcashd_pid, "-KILL");
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
/// supervisor to restart it, then verifies zcashd is responsive again **from a
/// new pid** — a response from the old, still-shutting-down zcashd RPC must not
/// count as recovery.
///
/// Only runs in managed (regtest) mode.
pub async fn zcashd_restarts_after_exit() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    let old_pid = setup.zcashd_pid()?;

    // Ask zcashd to stop gracefully; the zebrad supervisor should restart it.
    let _: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("stop", "[]")
        .await
        .map_err(|e| eyre!("zcashd stop: {e}"))?;

    // Recovery requires the RPC to answer from a *different* pid than the one
    // we stopped, so a lingering response from the old process cannot false-pass.
    let restart_result =
        super::reorg::wait_for_restarted_zcashd_rpc(&setup, old_pid, Duration::from_secs(60)).await;

    // Tear down before surfacing the result, so a failure never leaks the sidecar.
    setup.teardown()?;
    restart_result
}
