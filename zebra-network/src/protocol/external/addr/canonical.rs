//! Zebra's canonical node address format.
//!
//! Zebra canonicalises all received addresses into Rust [`PeerSocketAddr`]s.
//! If the address is an [IPv4-mapped IPv6 address], it becomes a [`SocketAddr::V4`]
//!
//! [IPv4-mapped IPv6 address]: https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use crate::PeerSocketAddr;

/// Transform a Zcash-deserialized [`Ipv6Addr`] into a canonical Zebra [`IpAddr`].
///
/// Zcash uses IPv6-mapped IPv4 addresses in its `addr` (v1) network messages.
/// Zebra converts those addresses to `Ipv4Addr`s, for maximum compatibility
/// with systems that don't understand IPv6.
///
/// Zebra also uses this canonical format for addresses from other sources.
pub(in super::super) fn canonical_ip_addr(v6_addr: &Ipv6Addr) -> IpAddr {
    use IpAddr::*;

    // if it is an IPv4-mapped address, convert to V4, otherwise leave it as V6
    v6_addr
        .to_ipv4_mapped()
        .map(V4)
        .unwrap_or_else(|| V6(*v6_addr))
}

/// Transform a [`SocketAddr`] into a canonical Zebra [`SocketAddr`], converting
/// IPv6-mapped IPv4 addresses, and removing IPv6 scope IDs and flow information.
///
/// Use [`canonical_peer_addr()`] and [`PeerSocketAddr`] for remote peer addresses,
/// so that Zebra doesn't log sensitive information about peers.
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

/// Transform a [`PeerSocketAddr`] into a canonical Zebra [`PeerSocketAddr`], converting
/// IPv6-mapped IPv4 addresses, and removing IPv6 scope IDs and flow information.
///
/// See [`canonical_ip_addr`] for detailed info on IPv6-mapped IPv4 addresses.
pub fn canonical_peer_addr(peer_socket_addr: impl Into<PeerSocketAddr>) -> PeerSocketAddr {
    let peer_socket_addr = peer_socket_addr.into();

    canonical_socket_addr(*peer_socket_addr).into()
}
