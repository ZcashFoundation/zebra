//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::{
    io::{self, Read},
    sync::Arc,
};

use crate::notes;
use crate::proofs::ZkSnarkProof;
use crate::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};
use crate::types::Script;

use super::*;

const OVERWINTER_VERSION_GROUP_ID: u32 = 0x03C4_8270;
const SAPLING_VERSION_GROUP_ID: u32 = 0x892F_2085;

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
            hash: TransactionHash(reader.read_32_bytes()?),
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
) -> Result<(BlockHeight, CoinbaseData), SerializationError> {
    match (data.get(0), data.len()) {
        // Blocks 1 through 16 inclusive encode block height with OP_N opcodes.
        (Some(op_n @ 0x51..=0x60), len) if len >= 1 => Ok((
            BlockHeight((op_n - 0x50) as u32),
            CoinbaseData(data.split_off(1)),
        )),
        // Blocks 17 through 256 exclusive encode block height with the `0x01` opcode.
        (Some(0x01), len) if len >= 2 => {
            Ok((BlockHeight(data[1] as u32), CoinbaseData(data.split_off(2))))
        }
        // Blocks 256 through 65536 exclusive encode block height with the `0x02` opcode.
        (Some(0x02), len) if len >= 3 => Ok((
            BlockHeight(data[1] as u32 + ((data[2] as u32) << 8)),
            CoinbaseData(data.split_off(3)),
        )),
        // Blocks 65536 through 2**24 exclusive encode block height with the `0x03` opcode.
        (Some(0x03), len) if len >= 4 => Ok((
            BlockHeight(data[1] as u32 + ((data[2] as u32) << 8) + ((data[3] as u32) << 16)),
            CoinbaseData(data.split_off(4)),
        )),
        // The genesis block does not encode the block height by mistake; special case it.
        // The first five bytes are [4, 255, 255, 7, 31], the little-endian encoding of
        // 520_617_983.  This is lucky because it means we can special-case the genesis block
        // while remaining below the maximum `BlockHeight` of 500_000_000 forced by `LockTime`.
        // While it's unlikely this code will ever process a block height that high, this means
        // we don't need to maintain a cascade of different invariants for allowable `BlockHeight`s.
        (Some(0x04), _) if data[..] == GENESIS_COINBASE_DATA[..] => {
            Ok((BlockHeight(0), CoinbaseData(data)))
        }
        // As noted above, this is included for completeness.
        (Some(0x04), len) if len >= 5 => {
            let h = data[1] as u32
                + ((data[2] as u32) << 8)
                + ((data[3] as u32) << 16)
                + ((data[4] as u32) << 24);
            if h < 500_000_000 {
                Ok((BlockHeight(h), CoinbaseData(data.split_off(5))))
            } else {
                Err(SerializationError::Parse("Invalid block height"))
            }
        }
        _ => Err(SerializationError::Parse(
            "Could not parse BIP34 height in coinbase data",
        )),
    }
}

fn coinbase_height_len(height: BlockHeight) -> usize {
    // We can't write this as a match statement on stable until exclusive range
    // guards are stabilized.
    if let 0 = height.0 {
        0
    } else if let _h @ 1..=16 = height.0 {
        1
    } else if let _h @ 17..=255 = height.0 {
        2
    } else if let _h @ 256..=65535 = height.0 {
        3
    } else if let _h @ 65536..=16_777_215 = height.0 {
        4
    } else if let _h @ 16_777_216..=499_999_999 = height.0 {
        5
    } else {
        panic!("Invalid coinbase height");
    }
}

fn write_coinbase_height<W: io::Write>(height: BlockHeight, mut w: W) -> Result<(), io::Error> {
    // We can't write this as a match statement on stable until exclusive range
    // guards are stabilized.
    if let 0 = height.0 {
        // Genesis block does not include height.
    } else if let h @ 1..=16 = height.0 {
        w.write_u8(0x50 + (h as u8))?;
    } else if let h @ 17..=255 = height.0 {
        w.write_u8(0x01)?;
        w.write_u8(h as u8)?;
    } else if let h @ 256..=65535 = height.0 {
        w.write_u8(0x02)?;
        w.write_u16::<LittleEndian>(h as u16)?;
    } else if let h @ 65536..=16_777_215 = height.0 {
        w.write_u8(0x03)?;
        w.write_u8(h as u8)?;
        w.write_u8((h >> 8) as u8)?;
        w.write_u8((h >> 16) as u8)?;
    } else if let h @ 16_777_216..=499_999_999 = height.0 {
        w.write_u8(0x04)?;
        w.write_u32::<LittleEndian>(h)?;
    } else {
        panic!("Invalid coinbase height");
    }
    Ok(())
}

impl ZcashSerialize for TransparentInput {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            TransparentInput::PrevOut {
                outpoint,
                script,
                sequence,
            } => {
                outpoint.zcash_serialize(&mut writer)?;
                script.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(*sequence)?;
            }
            TransparentInput::Coinbase {
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
                writer.write_all(&data.as_ref()[..])?;
                writer.write_u32::<LittleEndian>(*sequence)?;
            }
        }
        Ok(())
    }
}

impl ZcashDeserialize for TransparentInput {
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
            let mut data = Vec::with_capacity(len as usize);
            (&mut reader).take(len).read_to_end(&mut data)?;
            let (height, data) = parse_coinbase_height(data)?;
            let sequence = reader.read_u32::<LittleEndian>()?;
            Ok(TransparentInput::Coinbase {
                height,
                data,
                sequence,
            })
        } else {
            Ok(TransparentInput::PrevOut {
                outpoint: OutPoint {
                    hash: TransactionHash(bytes),
                    index: reader.read_u32::<LittleEndian>()?,
                },
                script: Script::zcash_deserialize(&mut reader)?,
                sequence: reader.read_u32::<LittleEndian>()?,
            })
        }
    }
}

impl ZcashSerialize for TransparentOutput {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u64::<LittleEndian>(self.value)?;
        self.pk_script.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for TransparentOutput {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(TransparentOutput {
            value: reader.read_u64::<LittleEndian>()?,
            pk_script: Script::zcash_deserialize(&mut reader)?,
        })
    }
}

impl<P: ZkSnarkProof> ZcashSerialize for JoinSplit<P> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u64::<LittleEndian>(self.vpub_old)?;
        writer.write_u64::<LittleEndian>(self.vpub_new)?;
        writer.write_all(&self.anchor[..])?;
        writer.write_all(&self.nullifiers[0][..])?;
        writer.write_all(&self.nullifiers[1][..])?;
        writer.write_all(&self.commitments[0][..])?;
        writer.write_all(&self.commitments[1][..])?;
        writer.write_all(&self.ephemeral_key.as_bytes()[..])?;
        writer.write_all(&self.random_seed[..])?;
        writer.write_all(&self.vmacs[0][..])?;
        writer.write_all(&self.vmacs[1][..])?;
        self.zkproof.zcash_serialize(&mut writer)?;
        self.enc_ciphertexts[0].zcash_serialize(&mut writer)?;
        self.enc_ciphertexts[1].zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl<P: ZkSnarkProof> ZcashDeserialize for JoinSplit<P> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(JoinSplit::<P> {
            vpub_old: reader.read_u64::<LittleEndian>()?,
            vpub_new: reader.read_u64::<LittleEndian>()?,
            anchor: reader.read_32_bytes()?,
            nullifiers: [reader.read_32_bytes()?, reader.read_32_bytes()?],
            commitments: [reader.read_32_bytes()?, reader.read_32_bytes()?],
            ephemeral_key: x25519_dalek::PublicKey::from(reader.read_32_bytes()?),
            random_seed: reader.read_32_bytes()?,
            vmacs: [reader.read_32_bytes()?, reader.read_32_bytes()?],
            zkproof: P::zcash_deserialize(&mut reader)?,
            enc_ciphertexts: [
                notes::sprout::EncryptedCiphertext::zcash_deserialize(&mut reader)?,
                notes::sprout::EncryptedCiphertext::zcash_deserialize(&mut reader)?,
            ],
        })
    }
}

impl<P: ZkSnarkProof> ZcashSerialize for JoinSplitData<P> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(self.joinsplits().count() as u64)?;
        for joinsplit in self.joinsplits() {
            joinsplit.zcash_serialize(&mut writer)?;
        }
        writer.write_all(&<[u8; 32]>::from(self.pub_key)[..])?;
        writer.write_all(&<[u8; 64]>::from(self.sig)[..])?;
        Ok(())
    }
}

impl<P: ZkSnarkProof> ZcashDeserialize for Option<JoinSplitData<P>> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let num_joinsplits = reader.read_compactsize()?;
        match num_joinsplits {
            0 => Ok(None),
            n => {
                let first = JoinSplit::zcash_deserialize(&mut reader)?;
                let mut rest = Vec::with_capacity((n - 1) as usize);
                for _ in 0..(n - 1) {
                    rest.push(JoinSplit::zcash_deserialize(&mut reader)?);
                }
                let pub_key = reader.read_32_bytes()?.into();
                let sig = reader.read_64_bytes()?.into();
                Ok(Some(JoinSplitData {
                    first,
                    rest,
                    pub_key,
                    sig,
                }))
            }
        }
    }
}

impl ZcashSerialize for Spend {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.cv[..])?;
        writer.write_all(&self.anchor.0[..])?;
        writer.write_all(&self.nullifier[..])?;
        writer.write_all(&<[u8; 32]>::from(self.rk)[..])?;
        self.zkproof.zcash_serialize(&mut writer)?;
        writer.write_all(&<[u8; 64]>::from(self.spend_auth_sig)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Spend {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        use crate::note_commitment_tree::SaplingNoteTreeRootHash;
        Ok(Spend {
            cv: reader.read_32_bytes()?,
            anchor: SaplingNoteTreeRootHash(reader.read_32_bytes()?),
            nullifier: reader.read_32_bytes()?,
            rk: reader.read_32_bytes()?.into(),
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
            spend_auth_sig: reader.read_64_bytes()?.into(),
        })
    }
}

impl ZcashSerialize for Output {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.cv[..])?;
        writer.write_all(&self.cmu[..])?;
        writer.write_all(&self.ephemeral_key.to_bytes())?;
        self.enc_ciphertext.zcash_serialize(&mut writer)?;
        self.out_ciphertext.zcash_serialize(&mut writer)?;
        self.zkproof.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Output {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Output {
            cv: reader.read_32_bytes()?,
            cmu: reader.read_32_bytes()?,
            ephemeral_key: jubjub::AffinePoint::from_bytes(reader.read_32_bytes()?).unwrap(),
            enc_ciphertext: notes::sapling::EncryptedCiphertext::zcash_deserialize(&mut reader)?,
            out_ciphertext: notes::sapling::OutCiphertext::zcash_deserialize(&mut reader)?,
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
        })
    }
}

impl ZcashSerialize for Transaction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            Transaction::V1 {
                inputs,
                outputs,
                lock_time,
            } => {
                writer.write_u32::<LittleEndian>(1)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
            }
            Transaction::V2 {
                inputs,
                outputs,
                lock_time,
                joinsplit_data,
            } => {
                writer.write_u32::<LittleEndian>(2)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
                match joinsplit_data {
                    // Write 0 for nJoinSplits to signal no JoinSplitData.
                    None => writer.write_compactsize(0)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }
            }
            Transaction::V3 {
                inputs,
                outputs,
                lock_time,
                expiry_height,
                joinsplit_data,
            } => {
                // Write version 3 and set the fOverwintered bit.
                writer.write_u32::<LittleEndian>(3 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(OVERWINTER_VERSION_GROUP_ID)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;
                match joinsplit_data {
                    // Write 0 for nJoinSplits to signal no JoinSplitData.
                    None => writer.write_compactsize(0)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }
            }
            Transaction::V4 {
                inputs,
                outputs,
                lock_time,
                expiry_height,
                value_balance,
                shielded_data,
                joinsplit_data,
            } => {
                // Write version 4 and set the fOverwintered bit.
                writer.write_u32::<LittleEndian>(4 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(SAPLING_VERSION_GROUP_ID)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;
                writer.write_i64::<LittleEndian>(*value_balance)?;

                // The previous match arms serialize in one go, because the
                // internal structure happens to nicely line up with the
                // serialized structure. However, this is not possible for
                // version 4 transactions, as the binding_sig for the
                // ShieldedData is placed at the end of the transaction. So
                // instead we have to interleave serialization of the
                // ShieldedData and the JoinSplitData.

                match shielded_data {
                    None => {
                        // Signal no shielded spends and no shielded outputs.
                        writer.write_compactsize(0)?;
                        writer.write_compactsize(0)?;
                    }
                    Some(shielded_data) => {
                        writer.write_compactsize(shielded_data.spends().count() as u64)?;
                        for spend in shielded_data.spends() {
                            spend.zcash_serialize(&mut writer)?;
                        }
                        writer.write_compactsize(shielded_data.outputs().count() as u64)?;
                        for output in shielded_data.outputs() {
                            output.zcash_serialize(&mut writer)?;
                        }
                    }
                }

                match joinsplit_data {
                    None => writer.write_compactsize(0)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }

                match shielded_data {
                    Some(sd) => writer.write_all(&<[u8; 64]>::from(sd.binding_sig)[..])?,
                    None => {}
                }
            }
        }
        Ok(())
    }
}

impl ZcashDeserialize for Transaction {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let (version, overwintered) = {
            const LOW_31_BITS: u32 = (1 << 31) - 1;
            let header = reader.read_u32::<LittleEndian>()?;
            (header & LOW_31_BITS, header >> 31 != 0)
        };

        // The overwintered flag MUST NOT be set for version 1 and 2 transactions.
        match (version, overwintered) {
            (1, false) => Ok(Transaction::V1 {
                inputs: Vec::zcash_deserialize(&mut reader)?,
                outputs: Vec::zcash_deserialize(&mut reader)?,
                lock_time: LockTime::zcash_deserialize(&mut reader)?,
            }),
            (2, false) => {
                // Version 2 transactions use Sprout-on-BCTV14.
                type OptV2JSD = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V2 {
                    inputs: Vec::zcash_deserialize(&mut reader)?,
                    outputs: Vec::zcash_deserialize(&mut reader)?,
                    lock_time: LockTime::zcash_deserialize(&mut reader)?,
                    joinsplit_data: OptV2JSD::zcash_deserialize(&mut reader)?,
                })
            }
            (3, true) => {
                let id = reader.read_u32::<LittleEndian>()?;
                if id != OVERWINTER_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse(
                        "expected OVERWINTER_VERSION_GROUP_ID",
                    ));
                }
                // Version 3 transactions use Sprout-on-BCTV14.
                type OptV3JSD = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V3 {
                    inputs: Vec::zcash_deserialize(&mut reader)?,
                    outputs: Vec::zcash_deserialize(&mut reader)?,
                    lock_time: LockTime::zcash_deserialize(&mut reader)?,
                    expiry_height: BlockHeight(reader.read_u32::<LittleEndian>()?),
                    joinsplit_data: OptV3JSD::zcash_deserialize(&mut reader)?,
                })
            }
            (4, true) => {
                let id = reader.read_u32::<LittleEndian>()?;
                if id != SAPLING_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse(
                        "expected SAPLING_VERSION_GROUP_ID",
                    ));
                }
                // Version 4 transactions use Sprout-on-Groth16.
                type OptV4JSD = Option<JoinSplitData<Groth16Proof>>;

                // The previous match arms deserialize in one go, because the
                // internal structure happens to nicely line up with the
                // serialized structure. However, this is not possible for
                // version 4 transactions, as the binding_sig for the
                // ShieldedData is placed at the end of the transaction. So
                // instead we have to pull the component parts out manually and
                // then assemble them.

                let inputs = Vec::zcash_deserialize(&mut reader)?;
                let outputs = Vec::zcash_deserialize(&mut reader)?;
                let lock_time = LockTime::zcash_deserialize(&mut reader)?;
                let expiry_height = BlockHeight(reader.read_u32::<LittleEndian>()?);
                let value_balance = reader.read_i64::<LittleEndian>()?;
                let mut shielded_spends = Vec::zcash_deserialize(&mut reader)?;
                let mut shielded_outputs = Vec::zcash_deserialize(&mut reader)?;
                let joinsplit_data = OptV4JSD::zcash_deserialize(&mut reader)?;

                use futures::future::Either::*;
                let shielded_data = if !shielded_spends.is_empty() {
                    Some(ShieldedData {
                        first: Left(shielded_spends.remove(0)),
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: reader.read_64_bytes()?.into(),
                    })
                } else if !shielded_outputs.is_empty() {
                    Some(ShieldedData {
                        first: Right(shielded_outputs.remove(0)),
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: reader.read_64_bytes()?.into(),
                    })
                } else {
                    None
                };

                Ok(Transaction::V4 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    value_balance,
                    shielded_data,
                    joinsplit_data,
                })
            }
            (_, _) => Err(SerializationError::Parse("bad tx header")),
        }
    }
}

impl<T> ZcashDeserialize for Arc<T>
where
    T: ZcashDeserialize,
{
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        Ok(Arc::new(T::zcash_deserialize(reader)?))
    }
}

impl<T> ZcashSerialize for Arc<T>
where
    T: ZcashSerialize,
{
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        T::zcash_serialize(self, writer)
    }
}
