//! Zcash `addr` (v1) message node address serialization.
//!
//! The [`AddrV1`] format serializes all IP addresses as IPv6 addresses.
//! IPv4 addresses are converted to an [IPv4-mapped IPv6 address] before serialization.
//!
//! [IPv4-mapped IPv6 address]: https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses

use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv6Addr, SocketAddrV6},
};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::serialization::{
    DateTime32, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashDeserializeInto,
    ZcashSerialize,
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{types::PeerServices, MAX_PROTOCOL_MESSAGE_LEN},
    PeerSocketAddr,
};

use super::canonical_peer_addr;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
use crate::protocol::external::arbitrary::canonical_peer_addr_strategy;

/// The first format used for Bitcoin node addresses.
/// Contains a node address, its advertised services, and last-seen time.
/// This struct is serialized and deserialized into `addr` (v1) messages.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub(in super::super) struct AddrV1 {
    /// The unverified "last seen time" gossiped by the remote peer that sent us
    /// this address.
    ///
    /// See the [`MetaAddr::last_seen`] method for details.
    untrusted_last_seen: DateTime32,

    /// The unverified services for the peer at `addr`.
    ///
    /// These services were advertised by the peer at `addr`,
    /// then gossiped via another peer.
    ///
    /// ## Security
    ///
    /// `untrusted_services` on gossiped peers may be invalid due to outdated
    /// records, older peer versions, or buggy or malicious peers.
    untrusted_services: PeerServices,

    /// The peer's canonical socket address.
    /// IPv4 addresses are serialized as an [IPv4-mapped IPv6 address].
    ///
    /// [IPv4-mapped IPv6 address]: https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(strategy = "canonical_peer_addr_strategy()")
    )]
    addr: PeerSocketAddr,
}

impl From<MetaAddr> for AddrV1 {
    fn from(meta_addr: MetaAddr) -> Self {
        let addr = canonical_peer_addr(meta_addr.addr);

        let untrusted_services = meta_addr.services.expect(
            "unexpected MetaAddr with missing peer services: \
             MetaAddrs should be sanitized before serialization",
        );
        let untrusted_last_seen = meta_addr.last_seen().expect(
            "unexpected MetaAddr with missing last seen time: \
             MetaAddrs should be sanitized before serialization",
        );

        AddrV1 {
            untrusted_last_seen,
            untrusted_services,
            addr,
        }
    }
}

impl From<AddrV1> for MetaAddr {
    fn from(addr: AddrV1) -> Self {
        MetaAddr::new_gossiped_meta_addr(
            addr.addr,
            addr.untrusted_services,
            addr.untrusted_last_seen,
        )
    }
}

impl ZcashSerialize for AddrV1 {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        self.untrusted_last_seen.zcash_serialize(&mut writer)?;
        writer.write_u64::<LittleEndian>(self.untrusted_services.bits())?;

        let ipv6_addr = ipv6_mapped_ip_addr(self.addr.ip());
        ipv6_addr.zcash_serialize(&mut writer)?;
        writer.write_u16::<BigEndian>(self.addr.port())?;

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
            addr: canonical_peer_addr(ipv6_addr),
            untrusted_services,
            untrusted_last_seen,
        })
    }
}

/// A serialized `addr` (v1) has a 4 byte time, 8 byte services, 16 byte IP addr, and 2 byte port
pub(in super::super) const ADDR_V1_SIZE: usize = 4 + 8 + 16 + 2;

impl TrustedPreallocate for AddrV1 {
    fn max_allocation() -> u64 {
        // Since ADDR_V1_SIZE is less than 2^5, the length of the largest list takes up 5 bytes.
        ((MAX_PROTOCOL_MESSAGE_LEN - 5) / ADDR_V1_SIZE) as u64
    }
}

/// Transform an `IpAddr` into an IPv6-mapped IPv4 addresses.
///
/// See [`canonical_ip_addr`] for detailed info on IPv6-mapped IPv4 addresses.
///
/// [`canonical_ip_addr`]: super::canonical::canonical_ip_addr
pub(in super::super) fn ipv6_mapped_ip_addr(ip_addr: IpAddr) -> Ipv6Addr {
    use IpAddr::*;

    match ip_addr {
        V4(v4_addr) => v4_addr.to_ipv6_mapped(),
        V6(v6_addr) => v6_addr,
    }
}
