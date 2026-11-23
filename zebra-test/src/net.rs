//! Network testing utility functions for Zebra.

use std::env;

/// The name of the env var that skips Zebra tests which need reliable,
/// fast network connectivity.
///
/// We use a constant so that the compiler detects typos.
const SKIP_NETWORK_TESTS: &str = "SKIP_NETWORK_TESTS";

/// The name of the env var that skips Zebra's IPv6 tests.
///
/// We use a constant so that the compiler detects typos.
const SKIP_IPV6_TESTS: &str = "SKIP_IPV6_TESTS";

/// Should we skip Zebra tests which need reliable, fast network connectivity?
//
// TODO: separate "good and reliable" from "any network"?
#[allow(clippy::print_stderr)]
pub fn zebra_skip_network_tests() -> bool {
    if env::var_os(SKIP_NETWORK_TESTS).is_some() {
        // This message is captured by the test runner, use
        // `cargo test -- --nocapture` to see it.
        eprintln!("Skipping network test because '$SKIP_NETWORK_TESTS' is set.");
        return true;
    }

    false
}

/// Should we skip Zebra tests which need a local IPv6 network stack and
/// IPv6 interface addresses?
///
/// Since `zebra_skip_network_tests` only disables tests which need reliable network connectivity,
/// we allow IPv6 tests even when `SKIP_NETWORK_TESTS` is set.
#[allow(clippy::print_stderr)]
pub fn zebra_skip_ipv6_tests() -> bool {
    if env::var_os(SKIP_IPV6_TESTS).is_some() {
        eprintln!("Skipping IPv6 network test because '$SKIP_IPV6_TESTS' is set.");
        return true;
    }

    // TODO: if we separate "good and reliable" from "any network",
    //       also skip IPv6 tests when we're skipping all network tests.
    false
}

/// Returns the "magic" port number that tells the operating system to
/// choose a random unallocated port.
///
/// The OS chooses a different port each time it opens a connection or
/// listener with this magic port number.
///
/// Bind the real listener using this port, then query its local address while keeping it open.
/// Do not probe and release a temporary listener: another process can take the released port.
pub fn random_unallocated_port() -> u16 {
    0
}
