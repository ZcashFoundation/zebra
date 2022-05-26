//! Zebra's canonical node address format.
//!
//! Zebra canonicalises all received addresses into Rust [`SocketAddr`]s.
//! If the address is an [IPv4-mapped IPv6 address], it becomes a [`SocketAddr::V4`]
//!
//! [IPv4-mapped IPv6 address]: https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

/// Transform a Zcash-deserialized IPv6 address into a canonical Zebra IP address.
///
/// Zcash uses IPv6-mapped IPv4 addresses in its `addr` (v1) network messages.
/// Zebra converts those addresses to `Ipv4Addr`s, for maximum compatibility
/// with systems that don't understand IPv6.
///
/// Zebra also uses this canonical format for addresses from other sources.
pub fn canonical_ip_addr(v6_addr: &Ipv6Addr) -> IpAddr {
    use IpAddr::*;

    // TODO: replace with `to_ipv4_mapped` when that stabilizes
    // <https://github.com/rust-lang/rust/issues/27709>
    match v6_addr.to_ipv4() {
        // workaround for unstable `to_ipv4_mapped`
        Some(v4_addr) if v4_addr.to_ipv6_mapped() == *v6_addr => V4(v4_addr),
        Some(_) | None => V6(*v6_addr),
    }
}

/// Transform a `SocketAddr` into a canonical Zebra `SocketAddr`, converting
/// IPv6-mapped IPv4 addresses, and removing IPv6 scope IDs and flow information.
///
/// See [`canonical_ip_addr`] for detailed info on IPv6-mapped IPv4 addresses.
pub fn canonical_socket_addr(socket_addr: impl Into<SocketAddr>) -> SocketAddr {
    use SocketAddr::*;

    let mut socket_addr = socket_addr.into();
    if let V6(v6_socket_addr) = socket_addr {
        let canonical_ip = canonical_ip_addr(v6_socket_addr.ip());
        // creating a new SocketAddr removes scope IDs and flow information
        socket_addr = SocketAddr::new(canonical_ip, socket_addr.port());
    }

    socket_addr
}
