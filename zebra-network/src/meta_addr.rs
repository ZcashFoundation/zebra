//! An address-with-metadata type used in Bitcoin networking.

use std::{
    cmp::{Ord, Ordering},
    io::{Read, Write},
    net::SocketAddr,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};

use zebra_chain::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};

use crate::protocol::types::PeerServices;

/// An address with metadata on its advertised services and last-seen time.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MetaAddr {
    /// The peer's address.
    pub addr: SocketAddr,
    /// The services advertised by the peer.
    pub services: PeerServices,
    /// When the peer was last seen.
    pub last_seen: DateTime<Utc>,
}

impl MetaAddr {
    /// Sanitize this `MetaAddr` before sending it to a remote peer.
    pub fn sanitize(mut self) -> MetaAddr {
        let interval = crate::constants::TIMESTAMP_TRUNCATION_SECONDS;
        let ts = self.last_seen.timestamp();
        self.last_seen = Utc.timestamp(ts - ts.rem_euclid(interval), 0);
        self
    }
}

impl Ord for MetaAddr {
    /// `MetaAddr`s are sorted newest-first, and then in an arbitrary
    /// but determinate total order.
    fn cmp(&self, other: &Self) -> Ordering {
        let newest_first = self.last_seen.cmp(&other.last_seen).reverse();
        newest_first.then_with(|| {
            // The remainder is meaningless as an ordering, but required so that we
            // have a total order on `MetaAddr` values: self and other must compare
            // as Ordering::Equal iff they are equal.
            use std::net::IpAddr::{V4, V6};
            match (self.addr.ip(), other.addr.ip()) {
                (V4(a), V4(b)) => a.octets().cmp(&b.octets()),
                (V6(a), V6(b)) => a.octets().cmp(&b.octets()),
                (V4(_), V6(_)) => Ordering::Less,
                (V6(_), V4(_)) => Ordering::Greater,
            }
            .then(self.addr.port().cmp(&other.addr.port()))
            .then(self.services.bits().cmp(&other.services.bits()))
        })
    }
}

impl PartialOrd for MetaAddr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl ZcashSerialize for MetaAddr {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u32::<LittleEndian>(self.last_seen.timestamp() as u32)?;
        writer.write_u64::<LittleEndian>(self.services.bits())?;
        writer.write_socket_addr(self.addr)?;
        Ok(())
    }
}

impl ZcashDeserialize for MetaAddr {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(MetaAddr {
            last_seen: Utc.timestamp(reader.read_u32::<LittleEndian>()? as i64, 0),
            // Discard unknown service bits.
            services: PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?),
            addr: reader.read_socket_addr()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // XXX remove this test and replace it with a proptest instance.
    #[test]
    fn sanitize_truncates_timestamps() {
        zebra_test::init();

        let entry = MetaAddr {
            services: PeerServices::default(),
            addr: "127.0.0.1:8233".parse().unwrap(),
            last_seen: Utc.timestamp(1_573_680_222, 0),
        }
        .sanitize();
        // We want the sanitized timestamp to be a multiple of the truncation interval.
        assert_eq!(
            entry.last_seen.timestamp() % crate::constants::TIMESTAMP_TRUNCATION_SECONDS,
            0
        );
    }
}
