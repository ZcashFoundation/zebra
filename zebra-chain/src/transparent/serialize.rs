//! Serializes and deserializes transparent data.

use std::io;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use zcash_script::{opcode::Evaluable, pattern};
use zcash_transparent::coinbase::{MAX_COINBASE_SCRIPT_LEN, MIN_COINBASE_SCRIPT_LEN};

use crate::{
    block::Height,
    serialization::{
        zcash_deserialize_bytes_external_count, CompactSizeMessage, ReadZcashExt,
        SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
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

/// Parses the BIP-34 block-height prefix of a non-genesis coinbase script and returns the height
/// along with the trailing miner data.
///
/// # Consensus
///
/// > A coinbase transaction for a block at block height greater than 0 MUST have a script that, as
/// > its first item, encodes the block height `height` as follows. For `height` in the range
/// > {1 .. 16}, the encoding is a single byte of value `0x50` + `height`. Otherwise, let
/// > `heightBytes` be the signed little-endian representation of `height`, using the minimum
/// > nonzero number of bytes such that the most significant byte is < `0x80`. The length of
/// > `heightBytes` MUST be in the range {1 .. 5}. Then the encoding is the length of `heightBytes`
/// > encoded as one byte, followed by `heightBytes` itself. This matches the encoding used by
/// > Bitcoin in the implementation of [BIP-34] (but the description here is to be considered
/// > normative).
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
///
/// [BIP-34]: <https://github.com/bitcoin/bips/blob/master/bip-0034.mediawiki>
///
/// # Strategy
///
/// Rather than parsing the height bytes ourselves, we read a candidate height directly off the
/// wire (using the prefix shape to locate the bytes), then re-encode it via
/// [`zcash_script::pattern::push_num`] — the same primitive used by
/// [`zcash_transparent::bundle::TxIn::coinbase`] to build coinbase inputs — and require byte-exact
/// equality. Any non-canonical input (wrong shape, non-minimal length, oversize, negative,
/// signed-bit games) fails this check.
fn parse_coinbase_height(script_sig: &[u8]) -> Result<(Height, Vec<u8>), SerializationError> {
    let parse_err = SerializationError::Parse;

    // Read a candidate height directly off the wire. The first byte tells us where the height
    // bytes are; we don't validate them yet — the oracle below catches any non-canonical input.
    let (h, len): (i64, usize) = match *script_sig
        .first()
        .ok_or(parse_err("Empty coinbase script"))?
    {
        op_n @ 0x51..=0x60 => (i64::from(op_n - 0x50), 1),
        n @ 1..=5 => {
            let bytes = script_sig
                .get(1..=usize::from(n))
                .ok_or(parse_err("Coinbase height push truncated"))?;
            // Permissive read: zero-extend the wire bytes into an i64. The candidate is only
            // trusted after the canonical-encode-and-compare check below.
            let mut buf = [0u8; 8];
            buf[..bytes.len()].copy_from_slice(bytes);
            (i64::from_le_bytes(buf), 1 + bytes.len())
        }
        _ => return Err(parse_err("Invalid coinbase script prefix")),
    };

    // Oracle: re-encode the candidate the way zcash_transparent's coinbase builder does, and
    // require byte-exact equality.
    if script_sig
        .get(..len)
        .ok_or(parse_err("Coinbase script too short"))?
        != pattern::push_num(h).to_bytes().as_slice()
    {
        return Err(parse_err("Non-canonical coinbase height encoding"));
    }

    let h = u32::try_from(h).map_err(|_| parse_err("Negative coinbase height"))?;
    let height =
        Height::try_from(h).map_err(|_| parse_err("Coinbase height exceeds Height::MAX"))?;

    Ok((height, script_sig[len..].to_vec()))
}

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

            // Read the coinbase script length and validate it against the consensus
            // bound *before* allocating any script bytes. The generic `Vec<u8>`
            // deserializer would otherwise allocate up to MAX_PROTOCOL_MESSAGE_LEN
            // bytes for an attacker-controlled CompactSize length and only reject
            // afterwards, letting a peer force multi-MiB transient allocations per
            // bogus block.
            //
            // # Consensus
            //
            // > A coinbase transaction script MUST have length in {2 .. 100} bytes.
            //
            // <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
            let len: CompactSizeMessage = (&mut reader).zcash_deserialize_into()?;
            let len: usize = len.into();
            if len < MIN_COINBASE_SCRIPT_LEN {
                return Err(SerializationError::Parse("Coinbase script is too short"));
            } else if len > MAX_COINBASE_SCRIPT_LEN {
                return Err(SerializationError::Parse("Coinbase script is too long"));
            }
            let script_sig = zcash_deserialize_bytes_external_count(len, &mut reader)?;

            let (height, data) = if script_sig.as_slice() == GENESIS_COINBASE_SCRIPT_SIG {
                (Height::MIN, GENESIS_COINBASE_SCRIPT_SIG.to_vec())
            } else {
                parse_coinbase_height(&script_sig)?
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
