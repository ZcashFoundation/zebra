//! Zcash `addrv2` message node address serialization.
//!
//! Zebra parses received IPv4 and IPv6 addresses in the [`AddrV2`] format.
//! But it ignores all other address types.
//!
//! Zebra never sends `addrv2` messages, because peers still accept `addr` (v1) messages.

use std::{
    convert::{TryFrom, TryInto},
    io::Read,
    net::{IpAddr, SocketAddr},
};

use byteorder::{BigEndian, ReadBytesExt};
use thiserror::Error;

use zebra_chain::serialization::{
    CompactSize64, DateTime32, SerializationError, TrustedPreallocate, ZcashDeserialize,
    ZcashDeserializeInto,
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{types::PeerServices, MAX_PROTOCOL_MESSAGE_LEN},
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(test)]
use byteorder::WriteBytesExt;
#[cfg(test)]
use std::io::Write;
#[cfg(test)]
use zebra_chain::serialization::{zcash_serialize_bytes, ZcashSerialize};

/// The maximum permitted size of the `addr` field in `addrv2` messages.
///
/// > Field addr has a variable length, with a maximum of 512 bytes (4096 bits).
/// > Clients MUST reject messages with a longer addr field, irrespective of the network ID.
///
/// <https://zips.z.cash/zip-0155#specification>
pub const MAX_ADDR_V2_ADDR_SIZE: usize = 512;

/// The network ID of [`Ipv4Addr`]s in `addrv2` messages.
///
/// > 0x01  IPV4  4   IPv4 address (globally routed internet)
///
/// <https://zips.z.cash/zip-0155#specification>
///
/// [`Ipv4Addr`]: std::net::Ipv4Addr
pub const ADDR_V2_IPV4_NETWORK_ID: u8 = 0x01;

/// The size of [`Ipv4Addr`]s in `addrv2` messages.
///
/// <https://zips.z.cash/zip-0155#specification>
///
/// [`Ipv4Addr`]: std::net::Ipv4Addr
pub const ADDR_V2_IPV4_ADDR_SIZE: usize = 4;

/// The network ID of [`Ipv6Addr`]s in `addrv2` messages.
///
/// > 0x02  IPV6  16  IPv6 address (globally routed internet)
///
/// <https://zips.z.cash/zip-0155#specification>
///
/// [`Ipv6Addr`]: std::net::Ipv6Addr
pub const ADDR_V2_IPV6_NETWORK_ID: u8 = 0x02;

/// The size of [`Ipv6Addr`]s in `addrv2` messages.
///
/// <https://zips.z.cash/zip-0155#specification>
///
/// [`Ipv6Addr`]: std::net::Ipv6Addr
pub const ADDR_V2_IPV6_ADDR_SIZE: usize = 16;

/// The second format used for Bitcoin node addresses.
/// Contains a node address, its advertised services, and last-seen time.
/// This struct is serialized and deserialized into `addrv2` messages.
///
/// [ZIP 155](https://zips.z.cash/zip-0155#specification)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub(in super::super) enum AddrV2 {
    /// An IPv4 or IPv6 node address, in `addrv2` format.
    IpAddr {
        /// The unverified "last seen time" gossiped by the remote peer that sent us
        /// this address.
        ///
        /// See the [`MetaAddr::last_seen`] method for details.
        untrusted_last_seen: DateTime32,

        /// The unverified services for the peer at `ip_addr`:`port`.
        ///
        /// These services were advertised by the peer at that address,
        /// then gossiped via another peer.
        ///
        /// ## Security
        ///
        /// `untrusted_services` on gossiped peers may be invalid due to outdated
        /// records, older peer versions, or buggy or malicious peers.
        untrusted_services: PeerServices,

        /// The peer's IP address.
        ///
        /// Unlike [`AddrV1`], this can be an IPv4 or IPv6 address.
        ///
        /// [`AddrV1`]: super::v1::AddrV1
        ip: IpAddr,

        /// The peer's TCP port.
        port: u16,
    },

    /// A node address with an unsupported `networkID`, in `addrv2` format.
    Unsupported,
}

// Just serialize in the tests for now.
//
// We can't guarantee that peers support addrv2 until it activates,
// and outdated peers are excluded from the network by a network upgrade.
// (Likely NU5 on mainnet, and NU6 on testnet.)
// https://zips.z.cash/zip-0155#deployment
//
// And Zebra doesn't use different codecs for different peer versions.
#[cfg(test)]
impl From<MetaAddr> for AddrV2 {
    fn from(meta_addr: MetaAddr) -> Self {
        let untrusted_services = meta_addr.services.expect(
            "unexpected MetaAddr with missing peer services: \
             MetaAddrs should be sanitized before serialization",
        );
        let untrusted_last_seen = meta_addr.last_seen().expect(
            "unexpected MetaAddr with missing last seen time: \
             MetaAddrs should be sanitized before serialization",
        );

        AddrV2::IpAddr {
            untrusted_last_seen,
            untrusted_services,
            ip: meta_addr.addr.ip(),
            port: meta_addr.addr.port(),
        }
    }
}

/// The error returned when converting `AddrV2::Unsupported` fails.
#[derive(Error, Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[error("can not parse this addrv2 variant: unimplemented or unrecognised AddrV2 network ID")]
pub struct UnsupportedAddrV2NetworkIdError;

impl TryFrom<AddrV2> for MetaAddr {
    type Error = UnsupportedAddrV2NetworkIdError;

    fn try_from(addr: AddrV2) -> Result<MetaAddr, UnsupportedAddrV2NetworkIdError> {
        if let AddrV2::IpAddr {
            untrusted_last_seen,
            untrusted_services,
            ip,
            port,
        } = addr
        {
            let addr = SocketAddr::new(ip, port);

            Ok(MetaAddr::new_gossiped_meta_addr(
                addr,
                untrusted_services,
                untrusted_last_seen,
            ))
        } else {
            Err(UnsupportedAddrV2NetworkIdError)
        }
    }
}

impl AddrV2 {
    /// Deserialize `addr_bytes` as an IPv4 or IPv6 address, using the `addrv2` format.
    /// Returns the corresponding [`IpAddr`].
    ///
    /// The returned IP version is chosen based on `IP_ADDR_SIZE`,
    /// which should be [`ADDR_V2_IPV4_ADDR_SIZE`] or [`ADDR_V2_IPV6_ADDR_SIZE`].
    #[allow(clippy::unwrap_in_result)]
    fn ip_addr_from_bytes<const IP_ADDR_SIZE: usize>(
        addr_bytes: Vec<u8>,
    ) -> Result<IpAddr, SerializationError>
    where
        IpAddr: From<[u8; IP_ADDR_SIZE]>,
    {
        // > Clients MUST reject messages that contain addresses that have
        // > a different length than specified in this table for a specific network ID,
        // > as these are meaningless.
        if addr_bytes.len() != IP_ADDR_SIZE {
            let error_msg = if IP_ADDR_SIZE == ADDR_V2_IPV4_ADDR_SIZE {
                "IP address field length did not match expected IPv4 address size in addrv2 message"
            } else if IP_ADDR_SIZE == ADDR_V2_IPV6_ADDR_SIZE {
                "IP address field length did not match expected IPv6 address size in addrv2 message"
            } else {
                unreachable!("unexpected IP address size when converting from bytes");
            };

            return Err(SerializationError::Parse(error_msg));
        };

        // > The IPV4 and IPV6 network IDs use addresses encoded in the usual way
        // > for binary IPv4 and IPv6 addresses in network byte order (big endian).
        let ip: [u8; IP_ADDR_SIZE] = addr_bytes.try_into().expect("just checked length");

        Ok(IpAddr::from(ip))
    }
}

// Just serialize in the tests for now.
//
// See the detailed note about ZIP-155 activation above.
#[cfg(test)]
impl ZcashSerialize for AddrV2 {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        if let AddrV2::IpAddr {
            untrusted_last_seen,
            untrusted_services,
            ip,
            port,
        } = self
        {
            // > uint32  Time that this node was last seen as connected to the network.
            untrusted_last_seen.zcash_serialize(&mut writer)?;

            // > Service bits. A CompactSize-encoded bit field that is 64 bits wide.
            let untrusted_services: CompactSize64 = untrusted_services.bits().into();
            untrusted_services.zcash_serialize(&mut writer)?;

            match ip {
                IpAddr::V4(ip) => {
                    // > Network identifier. An 8-bit value that specifies which network is addressed.
                    writer.write_u8(ADDR_V2_IPV4_NETWORK_ID)?;

                    // > The IPV4 and IPV6 network IDs use addresses encoded in the usual way
                    // > for binary IPv4 and IPv6 addresses in network byte order (big endian).
                    let ip: [u8; ADDR_V2_IPV4_ADDR_SIZE] = ip.octets();
                    // > CompactSize      The length in bytes of addr.
                    // > uint8[sizeAddr]  Network address. The interpretation depends on networkID.
                    zcash_serialize_bytes(&ip.to_vec(), &mut writer)?;

                    // > uint16  Network port. If not relevant for the network this MUST be 0.
                    writer.write_u16::<BigEndian>(*port)?;
                }
                IpAddr::V6(ip) => {
                    writer.write_u8(ADDR_V2_IPV6_NETWORK_ID)?;

                    let ip: [u8; ADDR_V2_IPV6_ADDR_SIZE] = ip.octets();
                    zcash_serialize_bytes(&ip.to_vec(), &mut writer)?;

                    writer.write_u16::<BigEndian>(*port)?;
                }
            }
        } else {
            unreachable!("unexpected AddrV2 variant: {:?}", self);
        }

        Ok(())
    }
}

/// Deserialize an `addrv2` entry according to:
/// <https://zips.z.cash/zip-0155#specification>
///
/// Unimplemented and unrecognised addresses are deserialized as [`AddrV2::Unsupported`].
/// (Deserialization consumes the correct number of bytes for unsupported addresses.)
impl ZcashDeserialize for AddrV2 {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        // > uint32  Time that this node was last seen as connected to the network.
        let untrusted_last_seen = (&mut reader).zcash_deserialize_into()?;

        // > Service bits. A CompactSize-encoded bit field that is 64 bits wide.
        let untrusted_services: CompactSize64 = (&mut reader).zcash_deserialize_into()?;
        let untrusted_services = PeerServices::from_bits_truncate(untrusted_services.into());

        // > Network identifier. An 8-bit value that specifies which network is addressed.
        //
        // See the list of reserved network IDs in ZIP 155.
        let network_id = reader.read_u8()?;

        // > CompactSize      The length in bytes of addr.
        // > uint8[sizeAddr]  Network address. The interpretation depends on networkID.
        let addr: Vec<u8> = (&mut reader).zcash_deserialize_into()?;

        // > uint16  Network port. If not relevant for the network this MUST be 0.
        let port = reader.read_u16::<BigEndian>()?;

        if addr.len() > MAX_ADDR_V2_ADDR_SIZE {
            return Err(SerializationError::Parse(
                "addr field longer than MAX_ADDR_V2_ADDR_SIZE in addrv2 message",
            ));
        }

        let ip = if network_id == ADDR_V2_IPV4_NETWORK_ID {
            AddrV2::ip_addr_from_bytes::<ADDR_V2_IPV4_ADDR_SIZE>(addr)?
        } else if network_id == ADDR_V2_IPV6_NETWORK_ID {
            AddrV2::ip_addr_from_bytes::<ADDR_V2_IPV6_ADDR_SIZE>(addr)?
        } else {
            // unimplemented or unrecognised network ID, just consume the bytes
            //
            // > Clients MUST NOT gossip addresses from unknown networks,
            // > because they have no means to validate those addresses
            // > and so can be tricked to gossip invalid addresses.

            return Ok(AddrV2::Unsupported);
        };

        Ok(AddrV2::IpAddr {
            untrusted_last_seen,
            untrusted_services,
            ip,
            port,
        })
    }
}

/// A serialized `addrv2` has:
/// * 4 byte time,
/// * 1-9 byte services,
/// * 1 byte networkID,
/// * 1-9 byte sizeAddr,
/// * 0-512 bytes addr,
/// * 2 bytes port.
#[allow(clippy::identity_op)]
pub(in super::super) const ADDR_V2_MIN_SIZE: usize = 4 + 1 + 1 + 1 + 0 + 2;

impl TrustedPreallocate for AddrV2 {
    fn max_allocation() -> u64 {
        // Since ADDR_V2_MIN_SIZE is less than 2^5, the length of the largest list takes up 5 bytes.
        ((MAX_PROTOCOL_MESSAGE_LEN - 5) / ADDR_V2_MIN_SIZE) as u64
    }
}
