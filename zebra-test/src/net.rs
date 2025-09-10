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

#[cfg(windows)]
/// Returns a random port number from the ephemeral port range.
///
/// Does not check if the port is already in use. It's impossible to do this
/// check in a reliable, cross-platform way.
///
/// ## Usage
///
/// If you want a once-off random unallocated port, use
/// `random_unallocated_port`. Don't use this function if you don't need
/// to - it has a small risk of port conflicts.
///
/// Use this function when you need to use the same random port multiple
/// times. For example: setting up both ends of a connection, or reusing
/// the same port multiple times.
pub fn random_known_port() -> u16 {
    use rand::Rng;
    // Use the intersection of the IANA/Windows/macOS ephemeral port range,
    // and the Linux ephemeral port range:
    //   - https://en.wikipedia.org/wiki/Ephemeral_port#Range
    // excluding ports less than 53500, to avoid:
    //   - typical Hyper-V reservations up to 52000:
    //      - https://github.com/googlevr/gvr-unity-sdk/issues/1002
    //      - https://github.com/docker/for-win/issues/3171
    //   - the MOM-Clear port 51515
    //      - https://docs.microsoft.com/en-us/troubleshoot/windows-server/networking/service-overview-and-network-port-requirements
    //   - the LDAP Kerberos byte-swapped reservation 53249
    //      - https://docs.microsoft.com/en-us/troubleshoot/windows-server/identity/ldap-kerberos-server-reset-tcp-sessions
    //   - macOS and Windows sequential ephemeral port allocations,
    //     starting from 49152:
    //      - https://dataplane.org/ephemeralports.html

    rand::thread_rng().gen_range(53500..60999)
}

#[cfg(not(windows))]
/// Uses the "magic" port number that tells the operating system to
/// choose a random unallocated port.
///
/// The OS chooses a different port each time it opens a connection or
/// listener with this magic port number.
///
/// Creates a TcpListener to find a random unallocated port, then drops the TcpListener to close the socket.
///
/// Returns the unallocated port number.
///
/// ## Usage
///
/// If you want a once-off random unallocated port, use
/// `random_unallocated_port`. Don't use this function if you don't need
/// to - it has a small risk of port conflicts when there is a delay
/// between this fn call and binding the tcp listener.
///
/// Use this function when you need to use the same random port multiple
/// times. For example: setting up both ends of a connection, or reusing
/// the same port multiple times.
///
/// ## Panics
///
/// If the OS finds no available ports
///
/// If there is an OS error when getting the socket address
pub fn random_known_port() -> u16 {
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

    let host_ip = Ipv4Addr::new(127, 0, 0, 1);
    let socket = TcpListener::bind(SocketAddrV4::new(host_ip, random_unallocated_port()))
        .expect("needs an available port")
        .local_addr()
        .expect("OS error: could not get socket addr");
    socket.port()
}

/// Returns the "magic" port number that tells the operating system to
/// choose a random unallocated port.
///
/// The OS chooses a different port each time it opens a connection or
/// listener with this magic port number.
///
/// ## Usage
///
/// See the usage note for `random_known_port`.
pub fn random_unallocated_port() -> u16 {
    0
}
