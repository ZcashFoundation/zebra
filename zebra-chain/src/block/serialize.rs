//! Serialization and deserialization for Zcash blocks.

use std::{borrow::Borrow, io};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{TimeZone, Utc};

use crate::{
    serialization::{
        CompactSizeMessage, ReadZcashExt, SerializationError, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
    work::{difficulty::CompactDifficulty, equihash},
};

use super::{header::ZCASH_BLOCK_VERSION, merkle, Block, CountedHeader, Hash, Header};

/// The maximum size of a Zcash block, in bytes.
///
/// Post-Sapling, this is also the maximum size of a transaction
/// in the Zcash specification. (But since blocks also contain a
/// block header and transaction count, the maximum size of a
/// transaction in the chain is approximately 1.5 kB smaller.)
pub const MAX_BLOCK_BYTES: u64 = 2_000_000;

impl ZcashSerialize for Header {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.version)?;
        self.previous_block_hash.zcash_serialize(&mut writer)?;
        writer.write_all(&self.merkle_root.0[..])?;
        writer.write_all(&self.commitment_bytes[..])?;
        writer.write_u32::<LittleEndian>(
            self.time
                .timestamp()
                .try_into()
                .expect("deserialized and generated timestamps are u32 values"),
        )?;
        writer.write_u32::<LittleEndian>(self.difficulty_threshold.0)?;
        writer.write_all(&self.nonce[..])?;
        self.solution.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Header {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // The Zcash specification says that
        // "The current and only defined block version number for Zcash is 4."
        // but this is not actually part of the consensus rules, and in fact
        // broken mining software created blocks that do not have version 4.
        // There are approximately 4,000 blocks with version 536870912; this
        // is the bit-reversal of the value 4, indicating that that mining pool
        // reversed bit-ordering of the version field. Because the version field
        // was not properly validated, these blocks were added to the chain.
        //
        // The only possible way to work around this is to do a similar hack
        // as the overwintered field in transaction parsing, which we do here:
        // treat the high bit (which zcashd interprets as a sign bit) as an
        // indicator that the version field is meaningful.
        //
        //
        let (version, future_version_flag) = {
            const LOW_31_BITS: u32 = (1 << 31) - 1;
            let raw_version = reader.read_u32::<LittleEndian>()?;
            (raw_version & LOW_31_BITS, raw_version >> 31 != 0)
        };

        if future_version_flag {
            return Err(SerializationError::Parse(
                "high bit was set in version field",
            ));
        }

        // # Consensus
        //
        // > The block version number MUST be greater than or equal to 4.
        //
        // https://zips.z.cash/protocol/protocol.pdf#blockheader
        if version < ZCASH_BLOCK_VERSION {
            return Err(SerializationError::Parse("version must be at least 4"));
        }

        Ok(Header {
            version,
            previous_block_hash: Hash::zcash_deserialize(&mut reader)?,
            merkle_root: merkle::Root(reader.read_32_bytes()?),
            commitment_bytes: reader.read_32_bytes()?,
            // This can't panic, because all u32 values are valid `Utc.timestamp`s
            time: Utc.timestamp(reader.read_u32::<LittleEndian>()?.into(), 0),
            difficulty_threshold: CompactDifficulty(reader.read_u32::<LittleEndian>()?),
            nonce: reader.read_32_bytes()?,
            solution: equihash::Solution::zcash_deserialize(reader)?,
        })
    }
}

impl ZcashSerialize for CountedHeader {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.header.zcash_serialize(&mut writer)?;

        // A header-only message has zero transactions in it.
        let transaction_count =
            CompactSizeMessage::try_from(0).expect("0 is below the message size limit");
        transaction_count.zcash_serialize(&mut writer)?;

        Ok(())
    }
}

impl ZcashDeserialize for CountedHeader {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let header = CountedHeader {
            header: (&mut reader).zcash_deserialize_into()?,
        };

        // We ignore the number of transactions in a header-only message,
        // it should always be zero.
        let _transaction_count: CompactSizeMessage = (&mut reader).zcash_deserialize_into()?;

        Ok(header)
    }
}

impl ZcashSerialize for Block {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // All block structs are validated when they are parsed.
        // So we don't need to check MAX_BLOCK_BYTES here, until
        // we start generating our own blocks (see #483).
        self.header.zcash_serialize(&mut writer)?;
        self.transactions.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Block {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        // # Consensus
        //
        // > The size of a block MUST be less than or equal to 2000000 bytes.
        //
        // https://zips.z.cash/protocol/protocol.pdf#blockheader
        //
        // If the limit is reached, we'll get an UnexpectedEof error
        let limited_reader = &mut reader.take(MAX_BLOCK_BYTES);
        Ok(Block {
            header: limited_reader.zcash_deserialize_into()?,
            transactions: limited_reader.zcash_deserialize_into()?,
        })
    }
}

/// A serialized block.
///
/// Stores bytes that are guaranteed to be deserializable into a [`Block`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SerializedBlock {
    bytes: Vec<u8>,
}

/// Build a [`SerializedBlock`] by serializing a block.
impl<B: Borrow<Block>> From<B> for SerializedBlock {
    fn from(block: B) -> Self {
        SerializedBlock {
            bytes: block
                .borrow()
                .zcash_serialize_to_vec()
                .expect("Writing to a `Vec` should never fail"),
        }
    }
}

/// Access the serialized bytes of a [`SerializedBlock`].
impl AsRef<[u8]> for SerializedBlock {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl From<Vec<u8>> for SerializedBlock {
    fn from(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}
