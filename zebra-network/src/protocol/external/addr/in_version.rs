//! Zcash `version` message node address serialization.
//!
//! The [`AddrInVersion`] format is the same as the `addr` ([`super::v1`]) message,
//! but without the timestamp field.

use std::{
    io::{Read, Write},
    net::{SocketAddr, SocketAddrV6},
};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::serialization::{
    SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
};

use crate::protocol::external::types::PeerServices;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
use crate::protocol::external::arbitrary::addr_v1_ipv6_mapped_socket_addr_strategy;

use super::{canonical_socket_addr, v1::ipv6_mapped_socket_addr};

/// The format used for Bitcoin node addresses in `version` messages.
/// Contains a node address and services, without a last-seen time.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct AddrInVersion {
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

    /// The peer's IPv6 socket address.
    /// IPv4 addresses are serialized as an [IPv4-mapped IPv6 address].
    ///
    /// [IPv4-mapped IPv6 address]: https://en.wikipedia.org/wiki/IPv6#IPv4-mapped_IPv6_addresses
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(strategy = "addr_v1_ipv6_mapped_socket_addr_strategy()")
    )]
    ipv6_addr: SocketAddrV6,
}

impl AddrInVersion {
    /// Returns a new `version` message address based on its fields.
    pub fn new(socket_addr: impl Into<SocketAddr>, untrusted_services: PeerServices) -> Self {
        Self {
            untrusted_services,
            ipv6_addr: ipv6_mapped_socket_addr(socket_addr),
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

impl ZcashSerialize for AddrInVersion {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u64::<LittleEndian>(self.untrusted_services.bits())?;

        self.ipv6_addr.ip().zcash_serialize(&mut writer)?;
        writer.write_u16::<BigEndian>(self.ipv6_addr.port())?;

        Ok(())
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
