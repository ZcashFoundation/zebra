//! Serializes and deserializes transparent data.

use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::{
    block::{self, Height},
    serialization::{
        zcash_serialize_bytes, FakeWriter, ReadZcashExt, SerializationError, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
    transaction,
};

use super::{CoinbaseData, Input, OutPoint, Output, Script};

/// The maximum length of the coinbase data.
///
/// Includes the encoded coinbase height, if any.
///
/// # Consensus
///
/// > A coinbase transaction script MUST have length in {2 .. 100} bytes.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub const MAX_COINBASE_DATA_LEN: usize = 100;

/// The maximum length of the encoded coinbase height.
///
/// # Consensus
///
/// > The length of heightBytes MUST be in the range {1 .. 5}. Then the encoding is the length
/// > of heightBytes encoded as one byte, followed by heightBytes itself.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub const MAX_COINBASE_HEIGHT_DATA_LEN: usize = 6;

/// The minimum length of the coinbase data.
///
/// Includes the encoded coinbase height, if any.
///
/// # Consensus
///
/// > A coinbase transaction script MUST have length in {2 .. 100} bytes.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub const MIN_COINBASE_DATA_LEN: usize = 2;

/// The coinbase data for a genesis block.
///
/// Zcash uses the same coinbase data for the Mainnet, Testnet, and Regtest
/// genesis blocks.
pub const GENESIS_COINBASE_DATA: [u8; 77] = [
    4, 255, 255, 7, 31, 1, 4, 69, 90, 99, 97, 115, 104, 48, 98, 57, 99, 52, 101, 101, 102, 56, 98,
    55, 99, 99, 52, 49, 55, 101, 101, 53, 48, 48, 49, 101, 51, 53, 48, 48, 57, 56, 52, 98, 54, 102,
    101, 97, 51, 53, 54, 56, 51, 97, 55, 99, 97, 99, 49, 52, 49, 97, 48, 52, 51, 99, 52, 50, 48,
    54, 52, 56, 51, 53, 100, 51, 52,
];

impl ZcashSerialize for OutPoint {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.hash.0[..])?;
        writer.write_u32::<LittleEndian>(self.index)?;
        Ok(())
    }
}

impl ZcashDeserialize for OutPoint {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(OutPoint {
            hash: transaction::Hash(reader.read_32_bytes()?),
            index: reader.read_u32::<LittleEndian>()?,
        })
    }
}

// Coinbase inputs include block heights (BIP34). These are not encoded
// directly, but as a Bitcoin script that pushes the block height to the stack
// when executed. The script data is otherwise unused. Because we want to
// *parse* transactions into an internal representation where illegal states are
// unrepresentable, we need just enough parsing of Bitcoin scripts to parse the
// coinbase height and split off the rest of the (inert) coinbase data.

// Starting at Network Upgrade 5, coinbase transactions also encode the block
// height in the expiry height field. But Zebra does not use this field to
// determine the coinbase height, because it is not present in older network
// upgrades.

/// Split `data` into a block height and remaining miner-controlled coinbase data.
///
/// The height may consume `0..=5` bytes at the stat of the coinbase data.
/// The genesis block does not include an encoded coinbase height.
///
/// # Consensus
///
/// > A coinbase transaction for a *block* at *block height* greater than 0 MUST have
/// > a script that, as its first item, encodes the *block height* `height` as follows.
/// > For `height` in the range {1..16}, the encoding is a single byte of value
/// > `0x50` + `height`. Otherwise, let `heightBytes` be the signed little-endian
/// > representation of `height`, using the minimum nonzero number of bytes such that
/// > the most significant byte is < `0x80`.
/// > The length of `heightBytes` MUST be in the range {1..5}.
/// > Then the encoding is the length of `heightBytes` encoded as one byte,
/// > followed by `heightBytes` itself. This matches the encoding used by Bitcoin in the
/// > implementation of [BIP-34] (but the description here is to be considered normative).
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
/// <https://github.com/bitcoin/bips/blob/master/bip-0034.mediawiki>
pub(crate) fn parse_coinbase_height(
    mut data: Vec<u8>,
) -> Result<(block::Height, CoinbaseData), SerializationError> {
    match (data.first(), data.len()) {
        // Blocks 1 through 16 inclusive encode block height with OP_N opcodes.
        (Some(op_n @ 0x51..=0x60), len) if len >= 1 => Ok((
            Height((op_n - 0x50) as u32),
            CoinbaseData(data.split_off(1)),
        )),
        // Blocks 17 through 128 exclusive encode block height with the `0x01` opcode.
        // The Bitcoin encoding requires that the most significant byte is below 0x80.
        (Some(0x01), len) if len >= 2 && data[1] < 0x80 => {
            let h = data[1] as u32;
            if (17..128).contains(&h) {
                Ok((Height(h), CoinbaseData(data.split_off(2))))
            } else {
                Err(SerializationError::Parse("Invalid block height"))
            }
        }
        // Blocks 128 through 32768 exclusive encode block height with the `0x02` opcode.
        // The Bitcoin encoding requires that the most significant byte is below 0x80.
        (Some(0x02), len) if len >= 3 && data[2] < 0x80 => {
            let h = data[1] as u32 + ((data[2] as u32) << 8);
            if (128..32_768).contains(&h) {
                Ok((Height(h), CoinbaseData(data.split_off(3))))
            } else {
                Err(SerializationError::Parse("Invalid block height"))
            }
        }
        // Blocks 32768 through 2**23 exclusive encode block height with the `0x03` opcode.
        // The Bitcoin encoding requires that the most significant byte is below 0x80.
        (Some(0x03), len) if len >= 4 && data[3] < 0x80 => {
            let h = data[1] as u32 + ((data[2] as u32) << 8) + ((data[3] as u32) << 16);
            if (32_768..8_388_608).contains(&h) {
                Ok((Height(h), CoinbaseData(data.split_off(4))))
            } else {
                Err(SerializationError::Parse("Invalid block height"))
            }
        }
        // The genesis block does not encode the block height by mistake; special case it.
        // The first five bytes are [4, 255, 255, 7, 31], the little-endian encoding of
        // 520_617_983.
        //
        // In the far future, Zcash might reach this height, and the miner might use the
        // same coinbase data as the genesis block. So we need an updated consensus rule
        // to handle this edge case.
        //
        // TODO: update this check based on the consensus rule changes in
        //       https://github.com/zcash/zips/issues/540
        (Some(0x04), _) if data[..] == GENESIS_COINBASE_DATA[..] => {
            Ok((Height(0), CoinbaseData(data)))
        }
        // As noted above, this is included for completeness.
        // The Bitcoin encoding requires that the most significant byte is below 0x80.
        (Some(0x04), len) if len >= 5 && data[4] < 0x80 => {
            let h = data[1] as u32
                + ((data[2] as u32) << 8)
                + ((data[3] as u32) << 16)
                + ((data[4] as u32) << 24);
            if (8_388_608..=Height::MAX.0).contains(&h) {
                Ok((Height(h), CoinbaseData(data.split_off(5))))
            } else {
                Err(SerializationError::Parse("Invalid block height"))
            }
        }
        _ => Err(SerializationError::Parse(
            "Could not parse BIP34 height in coinbase data",
        )),
    }
}

/// Encode `height` into a block height, as a prefix of the coinbase data.
/// Does not write `coinbase_data`.
///
/// The height may produce `0..=5` initial bytes of coinbase data.
///
/// # Errors
///
/// Returns an error if the coinbase height is zero,
/// and the `coinbase_data` does not match the Zcash mainnet and testnet genesis coinbase data.
/// (They are identical.)
///
/// This check is required, because the genesis block does not include an encoded
/// coinbase height,
pub(crate) fn write_coinbase_height<W: io::Write>(
    height: block::Height,
    coinbase_data: &CoinbaseData,
    mut w: W,
) -> Result<(), io::Error> {
    // We can't write this as a match statement on stable until exclusive range
    // guards are stabilized.
    // The Bitcoin encoding requires that the most significant byte is below 0x80,
    // so the ranges run up to 2^{n-1} rather than 2^n.
    if let 0 = height.0 {
        // The genesis block's coinbase data does not have a height prefix.
        // So we return an error if the entire coinbase data doesn't match genesis.
        // (If we don't do this check, then deserialization will fail.)
        //
        // TODO: update this check based on the consensus rule changes in
        //       https://github.com/zcash/zips/issues/540
        if coinbase_data.0 != GENESIS_COINBASE_DATA {
            return Err(io::Error::other("invalid genesis coinbase data"));
        }
    } else if let h @ 1..=16 = height.0 {
        w.write_u8(0x50 + (h as u8))?;
    } else if let h @ 17..=127 = height.0 {
        w.write_u8(0x01)?;
        w.write_u8(h as u8)?;
    } else if let h @ 128..=32_767 = height.0 {
        w.write_u8(0x02)?;
        w.write_u16::<LittleEndian>(h as u16)?;
    } else if let h @ 32_768..=8_388_607 = height.0 {
        w.write_u8(0x03)?;
        w.write_u8(h as u8)?;
        w.write_u8((h >> 8) as u8)?;
        w.write_u8((h >> 16) as u8)?;
    } else if let h @ 8_388_608..=block::Height::MAX_AS_U32 = height.0 {
        w.write_u8(0x04)?;
        w.write_u32::<LittleEndian>(h)?;
    } else {
        panic!("Invalid coinbase height");
    }
    Ok(())
}

impl Height {
    /// Get the size of `Height` when serialized into a coinbase input script.
    pub fn coinbase_zcash_serialized_size(&self) -> usize {
        let mut writer = FakeWriter(0);
        let empty_data = CoinbaseData(Vec::new());

        write_coinbase_height(*self, &empty_data, &mut writer).expect("writer should never fail");
        writer.0
    }
}

impl ZcashSerialize for Input {
    /// Serialize this transparent input.
    ///
    /// # Errors
    ///
    /// Returns an error if the coinbase height is zero,
    /// and the coinbase data does not match the Zcash mainnet and testnet genesis coinbase data.
    /// (They are identical.)
    ///
    /// This check is required, because the genesis block does not include an encoded
    /// coinbase height,
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            Input::PrevOut {
                outpoint,
                unlock_script,
                sequence,
            } => {
                outpoint.zcash_serialize(&mut writer)?;
                unlock_script.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(*sequence)?;
            }
            Input::Coinbase {
                height,
                data,
                sequence,
            } => {
                writer.write_all(&[0; 32][..])?;
                writer.write_u32::<LittleEndian>(0xffff_ffff)?;

                let mut height_and_data = Vec::new();
                write_coinbase_height(*height, data, &mut height_and_data)?;
                height_and_data.extend(&data.0);
                zcash_serialize_bytes(&height_and_data, &mut writer)?;

                writer.write_u32::<LittleEndian>(*sequence)?;
            }
        }
        Ok(())
    }
}

impl ZcashDeserialize for Input {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // This inlines the OutPoint deserialization to peek at the hash value
        // and detect whether we have a coinbase input.
        let bytes = reader.read_32_bytes()?;
        if bytes == [0; 32] {
            if reader.read_u32::<LittleEndian>()? != 0xffff_ffff {
                return Err(SerializationError::Parse("wrong index in coinbase"));
            }

            let data: Vec<u8> = (&mut reader).zcash_deserialize_into()?;

            // Check the coinbase data length.
            if data.len() > MAX_COINBASE_DATA_LEN {
                return Err(SerializationError::Parse("coinbase data is too long"));
            } else if data.len() < MIN_COINBASE_DATA_LEN {
                return Err(SerializationError::Parse("coinbase data is too short"));
            }

            let (height, data) = parse_coinbase_height(data)?;

            let sequence = reader.read_u32::<LittleEndian>()?;

            Ok(Input::Coinbase {
                height,
                data,
                sequence,
            })
        } else {
            Ok(Input::PrevOut {
                outpoint: OutPoint {
                    hash: transaction::Hash(bytes),
                    index: reader.read_u32::<LittleEndian>()?,
                },
                unlock_script: Script::zcash_deserialize(&mut reader)?,
                sequence: reader.read_u32::<LittleEndian>()?,
            })
        }
    }
}

impl ZcashSerialize for Output {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.value.zcash_serialize(&mut writer)?;
        self.lock_script.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Output {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let reader = &mut reader;

        Ok(Output {
            value: reader.zcash_deserialize_into()?,
            lock_script: Script::zcash_deserialize(reader)?,
        })
    }
}
