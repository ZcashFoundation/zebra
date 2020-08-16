use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{TimeZone, Utc};
use std::io;

use crate::serialization::ZcashDeserializeInto;
use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::work::{difficulty::CompactDifficulty, equihash};

use super::merkle::MerkleTreeRootHash;
use super::Block;
use super::Hash;
use super::Header;

/// The maximum size of a Zcash block, in bytes.
///
/// Post-Sapling, this is also the maximum size of a transaction
/// in the Zcash specification. (But since blocks also contain a
/// block header and transaction count, the maximum size of a
/// transaction in the chain is approximately 1.5 kB smaller.)
pub const MAX_BLOCK_BYTES: u64 = 2_000_000;

impl ZcashSerialize for Header {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.version)?;
        self.previous_block_hash.zcash_serialize(&mut writer)?;
        writer.write_all(&self.merkle_root_hash.0[..])?;
        writer.write_all(&self.root_bytes[..])?;
        // this is a truncating cast, rather than a saturating cast
        // but u32 times are valid until 2106, and our block verification time
        // checks should detect any truncation.
        writer.write_u32::<LittleEndian>(self.time.timestamp() as u32)?;
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
        if version < 4 {
            return Err(SerializationError::Parse("version must be at least 4"));
        }

        Ok(Header {
            version,
            previous_block_hash: Hash::zcash_deserialize(&mut reader)?,
            merkle_root_hash: MerkleTreeRootHash(reader.read_32_bytes()?),
            root_bytes: reader.read_32_bytes()?,
            // This can't panic, because all u32 values are valid `Utc.timestamp`s
            time: Utc.timestamp(reader.read_u32::<LittleEndian>()? as i64, 0),
            difficulty_threshold: CompactDifficulty(reader.read_u32::<LittleEndian>()?),
            nonce: reader.read_32_bytes()?,
            solution: equihash::Solution::zcash_deserialize(reader)?,
        })
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
        // If the limit is reached, we'll get an UnexpectedEof error
        let limited_reader = &mut reader.take(MAX_BLOCK_BYTES);
        Ok(Block {
            header: limited_reader.zcash_deserialize_into()?,
            transactions: limited_reader.zcash_deserialize_into()?,
        })
    }
}
