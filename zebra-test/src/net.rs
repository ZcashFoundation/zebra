//! Network testing utility functions for Zebra.

use rand::Rng;

/// The name of the env var that skips Zebra tests which need reliable,
/// fast network connectivity.
///
/// We use a constant so that the compiler detects typos.
//
// TODO: separate "good and reliable" from "any network"?
pub const ZEBRA_SKIP_NETWORK_TESTS: &str = "ZEBRA_SKIP_NETWORK_TESTS";

/// The name of the env var that skips Zebra's IPv6 tests.
///
/// We use a constant so that the compiler detects typos.
pub const ZEBRA_SKIP_IPV6_TESTS: &str = "ZEBRA_SKIP_IPV6_TESTS";

/// Returns a random port number from the ephemeral port range.
///
/// Does not check if the port is already in use. It's impossible to do this
/// check in a reliable, cross-platform way.
///
/// ## Usage
///
/// If you want a once-off random unallocated port, use
/// `random_unallocated_port`. Don't use this function if you don't need
/// to - it has a small risk of port conflcits.
///
/// Use this function when you need to use the same random port multiple
/// times. For example: setting up both ends of a connection, or re-using
/// the same port multiple times.
pub fn random_known_port() -> u16 {
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
