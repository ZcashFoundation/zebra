//! Opt-in Zakura dual-stack regtest e2e (lightweight, host-networked).
//!
//! Ignored by default, and additionally skipped in CI and when Docker is
//! unavailable, so it never gates normal CI — the unit-test lane runs ignored
//! tests (`--run-ignored=all`) on runners that have Docker, but this
//! host-networked docker-compose e2e is environment sensitive and is meant for
//! local validation only. There is no image build: each node runs the
//! host-built `zebrad` binary bind-mounted into stock `debian:trixie-slim`.
//!
//! To run it locally:
//!
//! ```sh
//! cargo test -p zebrad --test zakura_regtest_e2e -- --ignored --nocapture
//! ```
//!
//! To force it in a CI environment, set `ZAKURA_REGTEST_E2E=1`.
//!
//! It shells out to `docker/zakura-regtest-e2e/run.sh`, which builds `zebrad`
//! (debug) if needed, brings up four Regtest nodes sharing the host network — a
//! dual-stack seed, a pure Zakura-only node (`legacy_p2p = false`) that joins
//! only via the seed's `zakura.bootstrap_peers`, a legacy-only node, and a
//! dual-stack node that upgrades — and asserts legacy TCP backwards
//! compatibility, the legacy->Zakura upgrade handshake, and block propagation
//! to the pure-Zakura and legacy-only peers. The upgraded node4 propagation path
//! remains a documented P2 known issue and can be made fatal with
//! `ZAKURA_REGTEST_E2E_STRICT_UPGRADE=1`. See that script for the exact
//! assertions.

#![allow(clippy::print_stderr)]

use std::{path::PathBuf, process::Command};

#[ignore = "opt-in docker e2e: needs docker (builds the host zebrad binary itself)"]
#[test]
fn zakura_regtest_dual_stack_e2e() {
    // The unit-test CI lane runs ignored tests (`--run-ignored=all`) on runners
    // that have Docker, so `#[ignore]` plus the Docker guard below is not enough
    // to keep this host-networked, environment-sensitive e2e out of CI. Skip in
    // CI unless explicitly opted in with `ZAKURA_REGTEST_E2E=1`.
    if std::env::var_os("CI").is_some() && std::env::var_os("ZAKURA_REGTEST_E2E").is_none() {
        eprintln!("skipping Zakura regtest e2e in CI: set ZAKURA_REGTEST_E2E=1 to force it");
        return;
    }

    if !command_succeeds(Command::new("docker").arg("version")) {
        eprintln!("skipping Zakura regtest e2e: docker is unavailable");
        return;
    }

    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docker/zakura-regtest-e2e/run.sh");

    let status = Command::new("bash")
        .arg(&script)
        .status()
        .expect("failed to spawn the Zakura regtest e2e script");

    assert!(status.success(), "Zakura regtest dual-stack e2e failed");
}

fn command_succeeds(command: &mut Command) -> bool {
    command
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
