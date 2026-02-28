//! Serializes and deserializes transparent data.

use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use zcash_script::opcode::{self, Evaluable, PushValue};
use zcash_transparent::coinbase::{
    MAX_COINBASE_HEIGHT_LEN, MAX_COINBASE_SCRIPT_LEN, MIN_COINBASE_SCRIPT_LEN,
};

use crate::{
    block::Height,
    serialization::{
        ReadZcashExt, SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
    },
    transaction,
};

use super::{Input, OutPoint, Output, Script};

/// The coinbase data for a genesis block.
///
/// Zcash uses the same coinbase data for the Mainnet, Testnet, and Regtest
/// genesis blocks.
pub const GENESIS_COINBASE_SCRIPT_SIG: [u8; 77] = [
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
            Input::Coinbase { sequence, .. } => {
                // Write the null prevout.
                writer.write_all(&[0; 32][..])?;
                writer.write_u32::<LittleEndian>(0xffff_ffff)?;

                // Write the script sig containing the height and data.
                self.coinbase_script()
                    .ok_or_else(|| io::Error::other("invalid coinbase script sig"))?
                    .zcash_serialize(&mut writer)?;

                // Write the sequence.
                writer.write_u32::<LittleEndian>(*sequence)?;
            }
        }
        Ok(())
    }
}

impl ZcashDeserialize for Input {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // This inlines the OutPoint deserialization to peek at the hash value and detect whether we
        // have a coinbase input.
        let hash = reader.read_32_bytes()?;

        // Coinbase inputs have a null prevout hash.
        if hash == [0; 32] {
            // Coinbase txs have the prevout index set to `u32::MAX`.
            if reader.read_u32::<LittleEndian>()? != 0xffff_ffff {
                return Err(SerializationError::Parse("Wrong index in coinbase"));
            }

            let script_sig = Vec::zcash_deserialize(&mut reader)?;

            // # Consensus
            //
            // > A coinbase transaction script MUST have length in {2 .. 100} bytes.
            //
            // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
            if script_sig.len() < MIN_COINBASE_SCRIPT_LEN {
                return Err(SerializationError::Parse("Coinbase script is too short"));
            } else if script_sig.len() > MAX_COINBASE_SCRIPT_LEN {
                return Err(SerializationError::Parse("Coinbase script is too long"));
            }

            let (height, data) = if script_sig.as_slice() == GENESIS_COINBASE_SCRIPT_SIG {
                (Height::MIN, GENESIS_COINBASE_SCRIPT_SIG.to_vec())
            } else {
                // # Consensus
                //
                // > A coinbase transaction for a block at block height greater than 0 MUST have a script
                // > that, as its first item, encodes the block height `height` as follows. For `height` in
                // > the range {1 .. 16}, the encoding is a single byte of value `0x50` + `height`.
                // > Otherwise, let `heightBytes` be the signed little-endian representation of `height`,
                // > using the minimum nonzero number of bytes such that the most significant byte is <
                // > `0x80`. The length of `heightBytes` MUST be in the range {1 .. 5}. Then the encoding is
                // > the length of `heightBytes` encoded as one byte, followed by `heightBytes` itself. This
                // > matches the encoding used by Bitcoin in the implementation of [BIP-34] (but the
                // > description here is to be considered normative).
                //
                // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
                //
                // [BIP-34]: <https://github.com/bitcoin/bips/blob/master/bip-0034.mediawiki>
                let (height, data) = opcode::PossiblyBad::parse(&script_sig);
                let height = PushValue::restrict(height?)?;

                if height.byte_len() > MAX_COINBASE_HEIGHT_LEN {
                    Err(SerializationError::Parse(
                        "Coinbase height encoding is too long",
                    ))?
                }

                (height.to_num()?.try_into()?, data.to_vec())
            };

            Ok(Input::Coinbase {
                height,
                data,
                sequence: reader.read_u32::<LittleEndian>()?,
            })
        } else {
            Ok(Input::PrevOut {
                outpoint: OutPoint {
                    hash: transaction::Hash(hash),
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
