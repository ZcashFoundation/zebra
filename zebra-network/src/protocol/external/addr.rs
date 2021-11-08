//! Node address types and serialization for the Zcash wire format.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use zebra_chain::serialization::{
    ReadZcashExt, SerializationError, TrustedPreallocate, WriteZcashExt, ZcashDeserialize,
    ZcashDeserializeInto, ZcashSerialize,
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::{types::PeerServices, MAX_PROTOCOL_MESSAGE_LEN},
};

impl ZcashSerialize for MetaAddr {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        self.last_seen()
            .expect(
                "unexpected MetaAddr with missing last seen time: MetaAddrs should be sanitized \
                before serialization",
            )
            .zcash_serialize(&mut writer)?;

        writer.write_u64::<LittleEndian>(
            self.services
                .expect(
                    "unexpected MetaAddr with missing peer services: MetaAddrs should be \
                    sanitized before serialization",
                )
                .bits(),
        )?;

        writer.write_socket_addr(self.addr)?;

        Ok(())
    }
}

impl ZcashDeserialize for MetaAddr {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let untrusted_last_seen = (&mut reader).zcash_deserialize_into()?;
        let untrusted_services =
            PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?);
        let addr = reader.read_socket_addr()?;

        Ok(MetaAddr::new_gossiped_meta_addr(
            addr,
            untrusted_services,
            untrusted_last_seen,
        ))
    }
}

/// A serialized meta addr has a 4 byte time, 8 byte services, 16 byte IP addr, and 2 byte port
pub(super) const META_ADDR_SIZE: usize = 4 + 8 + 16 + 2;

impl TrustedPreallocate for MetaAddr {
    fn max_allocation() -> u64 {
        // Since a maximal serialized Vec<MetAddr> uses at least three bytes for its length (2MB  messages / 30B MetaAddr implies the maximal length is much greater than 253)
        // the max allocation can never exceed (MAX_PROTOCOL_MESSAGE_LEN - 3) / META_ADDR_SIZE
        ((MAX_PROTOCOL_MESSAGE_LEN - 3) / META_ADDR_SIZE) as u64
    }
}
