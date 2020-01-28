//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io;

use crate::proofs::ZkSnarkProof;
use crate::serialization::{
    ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
};
use crate::types::Script;

use super::*;

const OVERWINTER_VERSION_GROUP_ID: u32 = 0x03C48270;
const SAPLING_VERSION_GROUP_ID: u32 = 0x892F2085;

impl ZcashSerialize for OutPoint {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
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

impl ZcashSerialize for TransparentInput {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
        self.previous_output.zcash_serialize(&mut writer)?;
        self.signature_script.zcash_serialize(&mut writer)?;
        writer.write_u32::<LittleEndian>(self.sequence)?;
        Ok(())
    }
}

impl ZcashDeserialize for TransparentInput {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(TransparentInput {
            previous_output: OutPoint::zcash_deserialize(&mut reader)?,
            signature_script: Script::zcash_deserialize(&mut reader)?,
            sequence: reader.read_u32::<LittleEndian>()?,
        })
    }
}

impl ZcashSerialize for TransparentOutput {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
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
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
        writer.write_u64::<LittleEndian>(self.vpub_old)?;
        writer.write_u64::<LittleEndian>(self.vpub_new)?;
        writer.write_all(&self.anchor[..])?;
        writer.write_all(&self.nullifiers[0][..])?;
        writer.write_all(&self.nullifiers[1][..])?;
        writer.write_all(&self.commitments[0][..])?;
        writer.write_all(&self.commitments[1][..])?;
        writer.write_all(&self.ephemeral_key[..])?;
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
            ephemeral_key: reader.read_32_bytes()?,
            random_seed: reader.read_32_bytes()?,
            vmacs: [reader.read_32_bytes()?, reader.read_32_bytes()?],
            zkproof: P::zcash_deserialize(&mut reader)?,
            enc_ciphertexts: [
                joinsplit::EncryptedCiphertext::zcash_deserialize(&mut reader)?,
                joinsplit::EncryptedCiphertext::zcash_deserialize(&mut reader)?,
            ],
        })
    }
}

impl<P: ZkSnarkProof> ZcashSerialize for JoinSplitData<P> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
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

impl ZcashSerialize for SpendDescription {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
        writer.write_all(&self.cv[..])?;
        writer.write_all(&self.anchor.0[..])?;
        writer.write_all(&self.nullifier[..])?;
        writer.write_all(&<[u8; 32]>::from(self.rk)[..])?;
        self.zkproof.zcash_serialize(&mut writer)?;
        writer.write_all(&<[u8; 64]>::from(self.spend_auth_sig)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for SpendDescription {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        use crate::note_commitment_tree::SaplingNoteTreeRootHash;
        Ok(SpendDescription {
            cv: reader.read_32_bytes()?,
            anchor: SaplingNoteTreeRootHash(reader.read_32_bytes()?),
            nullifier: reader.read_32_bytes()?,
            rk: reader.read_32_bytes()?.into(),
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
            spend_auth_sig: reader.read_64_bytes()?.into(),
        })
    }
}

impl ZcashSerialize for OutputDescription {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
        writer.write_all(&self.cv[..])?;
        writer.write_all(&self.cmu[..])?;
        writer.write_all(&self.ephemeral_key[..])?;
        self.enc_ciphertext.zcash_serialize(&mut writer)?;
        self.out_ciphertext.zcash_serialize(&mut writer)?;
        self.zkproof.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for OutputDescription {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(OutputDescription {
            cv: reader.read_32_bytes()?,
            cmu: reader.read_32_bytes()?,
            ephemeral_key: reader.read_32_bytes()?,
            enc_ciphertext: shielded_data::EncryptedCiphertext::zcash_deserialize(&mut reader)?,
            out_ciphertext: shielded_data::OutCiphertext::zcash_deserialize(&mut reader)?,
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
        })
    }
}

impl ZcashSerialize for Transaction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
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
                let shielded_data = if shielded_spends.len() > 0 {
                    Some(ShieldedData {
                        first: Left(shielded_spends.remove(0)),
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: reader.read_64_bytes()?.into(),
                    })
                } else if shielded_outputs.len() > 0 {
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
