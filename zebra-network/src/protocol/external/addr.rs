//! Node address types and serialization for the Zcash wire format.

use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6},
};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::serialization::{
    DateTime32, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashDeserializeInto,
    ZcashSerialize,
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{types::PeerServices, MAX_PROTOCOL_MESSAGE_LEN},
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
use super::arbitrary::addr_v1_ipv6_mapped_socket_addr_strategy;

/// A version 1 node address, its advertised services, and last-seen time.
/// This struct is serialized and deserialized into `addr` messages.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub(super) struct AddrV1 {
    /// The peer's IPv6 socket address.
    /// IPv4 addresses are serialized as an [IPv4-mapped IPv6 address].
    ///
    /// [IPv4-mapped IPv6 address]: https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(strategy = "addr_v1_ipv6_mapped_socket_addr_strategy()")
    )]
    ipv6_addr: SocketAddrV6,

    /// The unverified services for the peer at `ipv6_addr`.
    ///
    /// These services were advertised by the peer at `ipv6_addr`,
    /// then gossiped via another peer.
    ///
    /// ## Security
    ///
    /// `untrusted_services` on gossiped peers may be invalid due to outdated
    /// records, older peer versions, or buggy or malicious peers.
    untrusted_services: PeerServices,

    /// The unverified "last seen time" gossiped by the remote peer that sent us
    /// this address.
    ///
    /// See the [`MetaAddr::last_seen`] method for details.
    untrusted_last_seen: DateTime32,
}

/// A version 1 node address and services, without a last-seen time.
/// This struct is serialized and deserialized as part of `version` messages.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct AddrInVersion {
    /// The peer's IPv6 socket address.
    /// IPv4 addresses are serialized as an [IPv4-mapped IPv6 address].
    ///
    /// [IPv4-mapped IPv6 address]: https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(strategy = "addr_v1_ipv6_mapped_socket_addr_strategy()")
    )]
    ipv6_addr: SocketAddrV6,

    /// The unverified services for the peer at `ipv6_addr`.
    ///
    /// These services were advertised by the peer at `ipv6_addr`,
    /// then gossiped via another peer.
    ///
    /// ## Security
    ///
    /// `untrusted_services` on gossiped peers may be invalid due to outdated
    /// records, older peer versions, or buggy or malicious peers.
    untrusted_services: PeerServices,
}

impl AddrInVersion {
    /// Returns a new `version` message address based on its fields.
    pub fn new(socket_addr: impl Into<SocketAddr>, untrusted_services: PeerServices) -> Self {
        Self {
            ipv6_addr: ipv6_mapped_socket_addr(socket_addr),
            untrusted_services,
        }
    }

    /// Returns the canonical address for this peer.
    pub fn addr(&self) -> SocketAddr {
        canonical_socket_addr(self.ipv6_addr)
    }

    /// Returns the services for this peer.
    pub fn untrusted_services(&self) -> PeerServices {
        self.untrusted_services
    }
}

impl From<MetaAddr> for AddrV1 {
    fn from(meta_addr: MetaAddr) -> Self {
        let ipv6_addr = ipv6_mapped_socket_addr(meta_addr.addr);

        let untrusted_services = meta_addr.services.expect(
            "unexpected MetaAddr with missing peer services: \
             MetaAddrs should be sanitized before serialization",
        );
        let untrusted_last_seen = meta_addr.last_seen().expect(
            "unexpected MetaAddr with missing last seen time: \
             MetaAddrs should be sanitized before serialization",
        );

        AddrV1 {
            ipv6_addr,
            untrusted_services,
            untrusted_last_seen,
        }
    }
}

impl From<AddrV1> for MetaAddr {
    fn from(addr_v1: AddrV1) -> Self {
        MetaAddr::new_gossiped_meta_addr(
            addr_v1.ipv6_addr.into(),
            addr_v1.untrusted_services,
            addr_v1.untrusted_last_seen,
        )
    }
}

impl ZcashSerialize for AddrV1 {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        self.untrusted_last_seen.zcash_serialize(&mut writer)?;
        writer.write_u64::<LittleEndian>(self.untrusted_services.bits())?;

        self.ipv6_addr.ip().zcash_serialize(&mut writer)?;
        writer.write_u16::<BigEndian>(self.ipv6_addr.port())?;

        Ok(())
    }
}

impl ZcashSerialize for AddrInVersion {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u64::<LittleEndian>(self.untrusted_services.bits())?;

        self.ipv6_addr.ip().zcash_serialize(&mut writer)?;
        writer.write_u16::<BigEndian>(self.ipv6_addr.port())?;

        Ok(())
    }
}

impl ZcashDeserialize for AddrV1 {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let untrusted_last_seen = (&mut reader).zcash_deserialize_into()?;
        let untrusted_services =
            PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?);

        let ipv6_addr = (&mut reader).zcash_deserialize_into()?;
        let port = reader.read_u16::<BigEndian>()?;

        // `0` is the default unspecified value for these fields.
        let ipv6_addr = SocketAddrV6::new(ipv6_addr, port, 0, 0);

        Ok(AddrV1 {
            ipv6_addr,
            untrusted_services,
            untrusted_last_seen,
        })
    }
}

impl ZcashDeserialize for AddrInVersion {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let untrusted_services =
            PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?);

        let ipv6_addr = (&mut reader).zcash_deserialize_into()?;
        let port = reader.read_u16::<BigEndian>()?;

        // `0` is the default unspecified value for these fields.
        let ipv6_addr = SocketAddrV6::new(ipv6_addr, port, 0, 0);

        Ok(AddrInVersion {
            ipv6_addr,
            untrusted_services,
        })
    }
}

/// A serialized `addr` (v1) has a 4 byte time, 8 byte services, 16 byte IP addr, and 2 byte port
pub(super) const ADDR_V1_SIZE: usize = 4 + 8 + 16 + 2;

impl TrustedPreallocate for AddrV1 {
    fn max_allocation() -> u64 {
        // Since a maximal serialized Vec<AddrV1> uses at least three bytes for its length
        // (2MB  messages / 30B MetaAddr implies the maximal length is much greater than 253)
        // the max allocation can never exceed (MAX_PROTOCOL_MESSAGE_LEN - 3) / META_ADDR_SIZE
        ((MAX_PROTOCOL_MESSAGE_LEN - 3) / ADDR_V1_SIZE) as u64
    }
}

/// Transform a Zcash-deserialized IPv6 address into a canonical Zebra IP address.
///
/// Zcash uses IPv6-mapped IPv4 addresses in its `addr` (v1) network messages.
/// Zebra converts those addresses to `Ipv4Addr`s, for maximum compatibility
/// with systems that don't understand IPv6.
pub fn canonical_ip_addr(v6_addr: &Ipv6Addr) -> IpAddr {
    use IpAddr::*;

    // TODO: replace with `to_ipv4_mapped` when that stabilizes
    // https://github.com/rust-lang/rust/issues/27709
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

/// Transform a `SocketAddr` into an IPv6-mapped IPv4 addresses.
///
/// See [`canonical_ip_addr`] for detailed info on IPv6-mapped IPv4 addresses.
pub fn ipv6_mapped_ip_addr(ip_addr: &IpAddr) -> Ipv6Addr {
    use IpAddr::*;

    match ip_addr {
        V4(v4_addr) => v4_addr.to_ipv6_mapped(),
        V6(v6_addr) => *v6_addr,
    }
}

/// Transform a `SocketAddr` into an IPv6-mapped IPv4 addresses,
/// for `addr` (v1) Zcash network messages.
///
/// Also remove IPv6 scope IDs and flow information.
///
/// See [`canonical_ip_addr`] for detailed info on IPv6-mapped IPv4 addresses.
pub fn ipv6_mapped_socket_addr(socket_addr: impl Into<SocketAddr>) -> SocketAddrV6 {
    let socket_addr = socket_addr.into();

    let ipv6_mapped_ip = ipv6_mapped_ip_addr(&socket_addr.ip());

    // Remove scope IDs and flow information.
    // `0` is the default unspecified value for these fields.
    SocketAddrV6::new(ipv6_mapped_ip, socket_addr.port(), 0, 0)
}
