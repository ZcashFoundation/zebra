//! An address-with-metadata type used in Bitcoin networking.

use std::io::{Read, Write};
use std::net::SocketAddr;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};

use zebra_chain::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};

use crate::protocol::types::PeerServices;

/// An address with metadata on its advertised services and last-seen time.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
// XXX determine whether we will use this struct in *our* networking handling
// code, or just in the definitions of the networking protocol (in which case
// it should live in the protocol submodule)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MetaAddr {
    /// The peer's address.
    pub addr: SocketAddr,
    /// The services advertised by the peer.
    pub services: PeerServices,
    /// When the peer was last seen.
    pub last_seen: DateTime<Utc>,
}

impl ZcashSerialize for MetaAddr {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), SerializationError> {
        writer.write_u32::<LittleEndian>(self.last_seen.timestamp() as u32)?;
        writer.write_u64::<LittleEndian>(self.services.0)?;
        writer.write_socket_addr(self.addr)?;
        Ok(())
    }
}

impl ZcashDeserialize for MetaAddr {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(MetaAddr {
            last_seen: Utc.timestamp(reader.read_u32::<LittleEndian>()? as i64, 0),
            services: PeerServices(reader.read_u64::<LittleEndian>()?),
            addr: reader.read_socket_addr()?,
        })
    }
}
