//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use std::{io, sync::Arc};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::{
    block::MAX_BLOCK_BYTES,
    parameters::{OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID, TX_V5_VERSION_GROUP_ID},
    primitives::{Groth16Proof, ZkSnarkProof},
    serialization::{
        zcash_deserialize_external_count, zcash_serialize_external_count, ReadZcashExt,
        SerializationError, TrustedPreallocate, WriteZcashExt, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
    sprout,
};

use super::*;
use sapling::Output;

impl ZcashDeserialize for jubjub::Fq {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let possible_scalar = jubjub::Fq::from_bytes(&reader.read_32_bytes()?);

        if possible_scalar.is_some().into() {
            Ok(possible_scalar.unwrap())
        } else {
            Err(SerializationError::Parse(
                "Invalid jubjub::Fq, input not canonical",
            ))
        }
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
                let first = sprout::JoinSplit::zcash_deserialize(&mut reader)?;
                let mut rest = Vec::with_capacity((n - 1) as usize);
                for _ in 0..(n - 1) {
                    rest.push(sprout::JoinSplit::zcash_deserialize(&mut reader)?);
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

impl ZcashSerialize for Transaction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // Post-Sapling, transaction size is limited to MAX_BLOCK_BYTES.
        // (Strictly, the maximum transaction size is about 1.5 kB less,
        // because blocks also include a block header.)
        //
        // Currently, all transaction structs are parsed as part of a
        // block. So we don't need to check transaction size here, until
        // we start parsing mempool transactions, or generating our own
        // transactions (see #483).
        //
        // Since we checkpoint on Canopy activation, we won't ever need
        // to check the smaller pre-Sapling transaction size limit.
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
                sapling_shielded_data,
                joinsplit_data,
            } => {
                // Write version 4 and set the fOverwintered bit.
                writer.write_u32::<LittleEndian>(4 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(SAPLING_VERSION_GROUP_ID)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;

                // The previous match arms serialize in one go, because the
                // internal structure happens to nicely line up with the
                // serialized structure. However, this is not possible for
                // version 4 transactions, as the binding_sig for the
                // ShieldedData is placed at the end of the transaction. So
                // instead we have to interleave serialization of the
                // ShieldedData and the JoinSplitData.

                match sapling_shielded_data {
                    None => {
                        // Signal no value balance.
                        writer.write_i64::<LittleEndian>(0)?;
                        // Signal no shielded spends and no shielded outputs.
                        writer.write_compactsize(0)?;
                        writer.write_compactsize(0)?;
                    }
                    Some(shielded_data) => {
                        shielded_data.value_balance.zcash_serialize(&mut writer)?;
                        writer.write_compactsize(shielded_data.spends().count() as u64)?;
                        for spend in shielded_data.spends() {
                            spend.zcash_serialize(&mut writer)?;
                        }
                        writer.write_compactsize(shielded_data.outputs().count() as u64)?;
                        for output in shielded_data
                            .outputs()
                            .cloned()
                            .map(sapling::OutputInTransactionV4)
                        {
                            output.zcash_serialize(&mut writer)?;
                        }
                    }
                }

                match joinsplit_data {
                    None => writer.write_compactsize(0)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }

                match sapling_shielded_data {
                    Some(sd) => writer.write_all(&<[u8; 64]>::from(sd.binding_sig)[..])?,
                    None => {}
                }
            }

            Transaction::V5 {
                lock_time,
                expiry_height,
                inputs,
                outputs,
                sapling_shielded_data,
                rest,
            } => {
                // header: Write version 5 and set the fOverwintered bit.
                writer.write_u32::<LittleEndian>(5 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(TX_V5_VERSION_GROUP_ID)?;

                // mining
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;

                // transparent
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;

                match sapling_shielded_data {
                    None => {
                        // nSpendSapling
                        writer.write_compactsize(0)?;
                        // nOutputSapling
                        writer.write_compactsize(0)?;
                    }
                    Some(shielded_data) => {
                        // Collect arrays for spends
                        let mut spend_prefixes =
                            Vec::<sapling::spend::SpendPrefixInTransactionV5>::new();
                        let mut spend_proofs = Vec::<Groth16Proof>::new();
                        let mut spend_sigs =
                            Vec::<redjubjub::Signature<redjubjub::SpendAuth>>::new();

                        let _ = shielded_data
                            .spends()
                            .map(|s| {
                                let (prefix, proof, sig) = s.clone().into_v5_parts();
                                spend_prefixes.push(prefix);
                                spend_proofs.push(proof);
                                spend_sigs.push(sig);
                            })
                            .collect::<Vec<_>>();

                        // Collect arrays for Outputs
                        let mut output_prefixes =
                            Vec::<sapling::output::OutputPrefixInTransactionV5>::new();
                        let mut output_proofs = Vec::<Groth16Proof>::new();

                        let _ = shielded_data
                            .outputs()
                            .map(|s| {
                                let (prefix, proof) = s.clone().into_v5_parts();
                                output_prefixes.push(prefix);
                                output_proofs.push(proof);
                            })
                            .collect::<Vec<_>>();

                        // nSpendsSapling - vSpendsSapling
                        spend_prefixes.zcash_serialize(&mut writer)?;
                        // nOutputsSapling - vOutputsSapling
                        output_prefixes.zcash_serialize(&mut writer)?;

                        // valueBalanceSapling
                        shielded_data.value_balance.zcash_serialize(&mut writer)?;

                        // anchorSapling
                        if spend_prefixes.len() > 0 {
                            writer.write_all(&<[u8; 32]>::from(shielded_data.shared_anchor)[..])?;
                        }
                        // vSpendProofSapling
                        zcash_serialize_external_count(&spend_proofs, &mut writer)?;
                        // vSpendAuthSigsSapling
                        zcash_serialize_external_count(&spend_sigs, &mut writer)?;

                        // vOutputProofSapling
                        zcash_serialize_external_count(&output_proofs, &mut writer)?;

                        // Serialize binding sig
                        writer.write_all(&<[u8; 64]>::from(shielded_data.binding_sig)[..])?;
                    }
                }
                // write the rest
                writer.write_all(rest)?;
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
                type OptV2Jsd = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V2 {
                    inputs: Vec::zcash_deserialize(&mut reader)?,
                    outputs: Vec::zcash_deserialize(&mut reader)?,
                    lock_time: LockTime::zcash_deserialize(&mut reader)?,
                    joinsplit_data: OptV2Jsd::zcash_deserialize(&mut reader)?,
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
                type OptV3Jsd = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V3 {
                    inputs: Vec::zcash_deserialize(&mut reader)?,
                    outputs: Vec::zcash_deserialize(&mut reader)?,
                    lock_time: LockTime::zcash_deserialize(&mut reader)?,
                    expiry_height: block::Height(reader.read_u32::<LittleEndian>()?),
                    joinsplit_data: OptV3Jsd::zcash_deserialize(&mut reader)?,
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
                type OptV4Jsd = Option<JoinSplitData<Groth16Proof>>;

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
                let expiry_height = block::Height(reader.read_u32::<LittleEndian>()?);

                let value_balance = (&mut reader).zcash_deserialize_into()?;
                let mut shielded_spends = Vec::zcash_deserialize(&mut reader)?;
                let mut shielded_outputs =
                    Vec::<sapling::OutputInTransactionV4>::zcash_deserialize(&mut reader)?
                        .into_iter()
                        .map(Output::from_v4)
                        .collect();

                let joinsplit_data = OptV4Jsd::zcash_deserialize(&mut reader)?;

                use futures::future::Either::*;
                // Arbitraily use a spend for `first`, if both are present
                let sapling_shielded_data = if !shielded_spends.is_empty() {
                    Some(sapling::ShieldedData {
                        value_balance,
                        shared_anchor: FieldNotPresent,
                        first: Left(shielded_spends.remove(0)),
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: reader.read_64_bytes()?.into(),
                    })
                } else if !shielded_outputs.is_empty() {
                    Some(sapling::ShieldedData {
                        value_balance,
                        shared_anchor: FieldNotPresent,
                        first: Right(shielded_outputs.remove(0)),
                        // the spends are actually empty here, but we use the
                        // vec for consistency and readability
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
                    sapling_shielded_data,
                    joinsplit_data,
                })
            }
            (5, true) => {
                // Deserialize header
                let id = reader.read_u32::<LittleEndian>()?;
                if id != TX_V5_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse("expected TX_V5_VERSION_GROUP_ID"));
                }

                // Deserialize mining
                let lock_time = LockTime::zcash_deserialize(&mut reader)?;
                let expiry_height = block::Height(reader.read_u32::<LittleEndian>()?);

                // Deserialize transparent
                let inputs = Vec::zcash_deserialize(&mut reader)?;
                let outputs = Vec::zcash_deserialize(&mut reader)?;

                // nSpendsSapling - vSpendsSapling
                let spend_prefixes =
                    Vec::<sapling::spend::SpendPrefixInTransactionV5>::zcash_deserialize(
                        &mut reader,
                    )?;
                // nOutputsSapling - vOutputsSapling
                let output_prefixes =
                    Vec::<sapling::output::OutputPrefixInTransactionV5>::zcash_deserialize(
                        &mut reader,
                    )?;

                // Cretate counters
                let spends_count = spend_prefixes.len();
                let outputs_count = output_prefixes.len();

                // valueBalanceSapling
                let mut value_balance = None;
                if spends_count > 0 || outputs_count > 0 {
                    value_balance = Some((&mut reader).zcash_deserialize_into()?);
                }
                // anchorSapling
                let mut shared_anchor = None;
                if spends_count > 0 {
                    shared_anchor = Some(sapling::tree::Root(reader.read_32_bytes()?));
                }

                // vSpendsProofSapling
                let spend_proofs: Vec<Groth16Proof> =
                    zcash_deserialize_external_count(spends_count, &mut reader)?;
                // vSpendAuthSigSapling
                let spend_sigs: Vec<redjubjub::Signature<redjubjub::SpendAuth>> =
                    zcash_deserialize_external_count(spends_count, &mut reader)?;

                // vOutputProofsSapling
                let output_proofs: Vec<Groth16Proof> =
                    zcash_deserialize_external_count(outputs_count, &mut reader)?;

                // bindingSigSapling
                let mut binding_sig: Option<redjubjub::Signature<redjubjub::Binding>> = None;
                if spends_count > 0 || outputs_count > 0 {
                    binding_sig = Some(reader.read_64_bytes()?.into());
                }

                // Create shielded spends from deserialized parts
                let mut shielded_spends = Vec::<sapling::Spend<sapling::SharedAnchor>>::new();
                if spends_count > 0 {
                    for i in 0..spends_count {
                        shielded_spends.push(sapling::Spend::from_v5_parts(
                            spend_prefixes[i].clone(),
                            spend_proofs[i],
                            spend_sigs[i],
                        ));
                    }
                }

                // Create shielded outputs from deserialized parts
                let mut shielded_outputs = Vec::<sapling::Output>::new();
                if outputs_count > 0 {
                    for i in 0..outputs_count {
                        shielded_outputs.push(sapling::Output::from_v5_parts(
                            output_prefixes[i].clone(),
                            output_proofs[i],
                        ));
                    }
                }

                // Create shielded data
                use futures::future::Either::*;
                // Arbitraily use a spend for `first`, if both are present
                let sapling_shielded_data = if spends_count > 0 {
                    Some(sapling::ShieldedData {
                        value_balance: value_balance.expect("present when spends_count > 0"),
                        shared_anchor: shared_anchor.expect("present when spends_count > 0"),
                        first: Left(shielded_spends.remove(0)),
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: binding_sig.expect("present when spends_count > 0"),
                    })
                } else if outputs_count > 0 {
                    Some(sapling::ShieldedData {
                        value_balance: value_balance.expect("present when outputs_count > 0"),
                        // TODO: delete shared anchor when there are no spends
                        shared_anchor: shared_anchor.unwrap_or_default(),
                        first: Right(shielded_outputs.remove(0)),
                        // the spends are actually empty here, but we use the
                        // vec for consistency and readability
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: binding_sig.expect("present when outputs_count > 0"),
                    })
                } else {
                    None
                };

                // Deserialize the rest of the transaction
                let mut rest = Vec::new();
                reader.read_to_end(&mut rest)?;

                Ok(Transaction::V5 {
                    lock_time,
                    expiry_height,
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    rest,
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

/// A Tx Input must have an Outpoint (32 byte hash + 4 byte index), a 4 byte sequence number,
/// and a signature script, which always takes a min of 1 byte (for a length 0 script)
pub(crate) const MIN_TRANSPARENT_INPUT_SIZE: u64 = 32 + 4 + 4 + 1;
/// A Transparent output has an 8 byte value and script which takes a min of 1 byte
pub(crate) const MIN_TRANSPARENT_OUTPUT_SIZE: u64 = 8 + 1;
/// All txs must have at least one input, a 4 byte locktime, and at least one output
pub(crate) const MIN_TRANSPARENT_TX_SIZE: u64 =
    MIN_TRANSPARENT_INPUT_SIZE + 4 + MIN_TRANSPARENT_OUTPUT_SIZE;

/// No valid Zcash message contains more transactions than can fit in a single block
///
/// `tx` messages contain a single transaction, and `block` messages are limited to the maximum
/// block size.
impl TrustedPreallocate for Arc<Transaction> {
    fn max_allocation() -> u64 {
        // A transparent transaction is the smallest transaction variant
        MAX_BLOCK_BYTES / MIN_TRANSPARENT_TX_SIZE
    }
}

/// The maximum number of inputs in a valid Zcash on-chain transaction.
///
/// If a transaction contains more inputs than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus.
impl TrustedPreallocate for transparent::Input {
    fn max_allocation() -> u64 {
        MAX_BLOCK_BYTES / MIN_TRANSPARENT_INPUT_SIZE
    }
}

/// The maximum number of outputs in a valid Zcash on-chain transaction.
///
/// If a transaction contains more outputs than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus.
impl TrustedPreallocate for transparent::Output {
    fn max_allocation() -> u64 {
        MAX_BLOCK_BYTES / MIN_TRANSPARENT_OUTPUT_SIZE
    }
}
