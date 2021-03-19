use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::{
    block,
    serialization::{
        ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashDeserializeInto,
        ZcashSerialize,
    },
    transaction,
};

use super::{CoinbaseData, Input, OutPoint, Output, Script};

/// The coinbase data for a genesis block.
///
/// Zcash uses the same coinbase data for the Mainnet, Testnet, and Regtest
/// genesis blocks.
const GENESIS_COINBASE_DATA: [u8; 77] = [
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

fn parse_coinbase_height(
    mut data: Vec<u8>,
) -> Result<(block::Height, CoinbaseData), SerializationError> {
    use block::Height;
    match (data.get(0), data.len()) {
        // Blocks 1 through 16 inclusive encode block height with OP_N opcodes.
        (Some(op_n @ 0x51..=0x60), len) if len >= 1 => Ok((
            Height((op_n - 0x50) as u32),
            CoinbaseData(data.split_off(1)),
        )),
        // Blocks 17 through 128 exclusive encode block height with the `0x01` opcode.
        // The Bitcoin encoding requires that the most significant byte is below 0x80.
        (Some(0x01), len) if len >= 2 && data[1] < 0x80 => {
            Ok((Height(data[1] as u32), CoinbaseData(data.split_off(2))))
        }
        // Blocks 128 through 32768 exclusive encode block height with the `0x02` opcode.
        // The Bitcoin encoding requires that the most significant byte is below 0x80.
        (Some(0x02), len) if len >= 3 && data[2] < 0x80 => Ok((
            Height(data[1] as u32 + ((data[2] as u32) << 8)),
            CoinbaseData(data.split_off(3)),
        )),
        // Blocks 65536 through 2**23 exclusive encode block height with the `0x03` opcode.
        // The Bitcoin encoding requires that the most significant byte is below 0x80.
        (Some(0x03), len) if len >= 4 && data[3] < 0x80 => Ok((
            Height(data[1] as u32 + ((data[2] as u32) << 8) + ((data[3] as u32) << 16)),
            CoinbaseData(data.split_off(4)),
        )),
        // The genesis block does not encode the block height by mistake; special case it.
        // The first five bytes are [4, 255, 255, 7, 31], the little-endian encoding of
        // 520_617_983.  This is lucky because it means we can special-case the genesis block
        // while remaining below the maximum `block::Height` of 500_000_000 forced by `LockTime`.
        // While it's unlikely this code will ever process a block height that high, this means
        // we don't need to maintain a cascade of different invariants for allowable heights.
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
            if h <= Height::MAX.0 {
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

fn coinbase_height_len(height: block::Height) -> usize {
    // We can't write this as a match statement on stable until exclusive range
    // guards are stabilized.
    if let 0 = height.0 {
        0
    } else if let _h @ 1..=16 = height.0 {
        1
    } else if let _h @ 17..=127 = height.0 {
        2
    } else if let _h @ 128..=32767 = height.0 {
        3
    } else if let _h @ 32768..=8_388_607 = height.0 {
        4
    } else if let _h @ 8_388_608..=block::Height::MAX_AS_U32 = height.0 {
        5
    } else {
        panic!("Invalid coinbase height");
    }
}

fn write_coinbase_height<W: io::Write>(height: block::Height, mut w: W) -> Result<(), io::Error> {
    // We can't write this as a match statement on stable until exclusive range
    // guards are stabilized.
    // The Bitcoin encoding requires that the most significant byte is below 0x80,
    // so the ranges run up to 2^{n-1} rather than 2^n.
    if let 0 = height.0 {
        // Genesis block does not include height.
    } else if let h @ 1..=16 = height.0 {
        w.write_u8(0x50 + (h as u8))?;
    } else if let h @ 17..=127 = height.0 {
        w.write_u8(0x01)?;
        w.write_u8(h as u8)?;
    } else if let h @ 128..=32767 = height.0 {
        w.write_u8(0x02)?;
        w.write_u16::<LittleEndian>(h as u16)?;
    } else if let h @ 32768..=8_388_607 = height.0 {
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

impl ZcashSerialize for Input {
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
                let height_len = coinbase_height_len(*height);
                let total_len = height_len + data.as_ref().len();
                writer.write_compactsize(total_len as u64)?;
                write_coinbase_height(*height, &mut writer)?;
                writer.write_all(data.as_ref())?;
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
            let len = reader.read_compactsize()?;
            if len > 100 {
                return Err(SerializationError::Parse("coinbase has too much data"));
            }
            let mut data = vec![0; len as usize];
            reader.read_exact(&mut data[..])?;
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
