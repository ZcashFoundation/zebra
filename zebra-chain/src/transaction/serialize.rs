//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use std::{convert::TryInto, io, sync::Arc};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use halo2::{arithmetic::FieldExt, pasta::pallas};

use crate::{
    amount,
    block::MAX_BLOCK_BYTES,
    parameters::{OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID, TX_V5_VERSION_GROUP_ID},
    primitives::{
        redpallas::{Binding, Signature, SpendAuth},
        Groth16Proof, Halo2Proof, ZkSnarkProof,
    },
    serialization::{
        zcash_deserialize_external_count, zcash_serialize_external_count, AtLeastOne, ReadZcashExt,
        SerializationError, TrustedPreallocate, WriteZcashExt, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
    sprout,
};

use super::*;
use sapling::{Output, SharedAnchor, Spend};

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

impl ZcashDeserialize for pallas::Scalar {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let possible_scalar = pallas::Scalar::from_bytes(&reader.read_32_bytes()?);

        if possible_scalar.is_some().into() {
            Ok(possible_scalar.unwrap())
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Scalar, input not canonical",
            ))
        }
    }
}

impl ZcashDeserialize for pallas::Base {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let possible_field_element = pallas::Base::from_bytes(&reader.read_32_bytes()?);

        if possible_field_element.is_some().into() {
            Ok(possible_field_element.unwrap())
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Base, input not canonical",
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

// Transaction::V5 serializes sapling ShieldedData in a single continuous byte
// range, so we can implement its serialization and deserialization separately.
// (Unlike V4, where it must be serialized as part of the transaction.)

impl ZcashSerialize for Option<sapling::ShieldedData<SharedAnchor>> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            None => {
                // nSpendsSapling
                writer.write_compactsize(0)?;
                // nOutputsSapling
                writer.write_compactsize(0)?;
            }
            Some(sapling_shielded_data) => {
                sapling_shielded_data.zcash_serialize(&mut writer)?;
            }
        }
        Ok(())
    }
}

impl ZcashSerialize for sapling::ShieldedData<SharedAnchor> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // Collect arrays for Spends
        // There's no unzip3, so we have to unzip twice.
        let (spend_prefixes, spend_proofs_sigs): (Vec<_>, Vec<_>) = self
            .spends()
            .cloned()
            .map(sapling::Spend::<SharedAnchor>::into_v5_parts)
            .map(|(prefix, proof, sig)| (prefix, (proof, sig)))
            .unzip();
        let (spend_proofs, spend_sigs) = spend_proofs_sigs.into_iter().unzip();

        // Collect arrays for Outputs
        let (output_prefixes, output_proofs): (Vec<_>, _) =
            self.outputs().cloned().map(Output::into_v5_parts).unzip();

        // nSpendsSapling and vSpendsSapling
        spend_prefixes.zcash_serialize(&mut writer)?;
        // nOutputsSapling and vOutputsSapling
        output_prefixes.zcash_serialize(&mut writer)?;

        // valueBalanceSapling
        self.value_balance.zcash_serialize(&mut writer)?;

        // anchorSapling
        // `TransferData` ensures this field is only present when there is at
        // least one spend.
        if let Some(shared_anchor) = self.shared_anchor() {
            writer.write_all(&<[u8; 32]>::from(shared_anchor)[..])?;
        }

        // vSpendProofsSapling
        zcash_serialize_external_count(&spend_proofs, &mut writer)?;
        // vSpendAuthSigsSapling
        zcash_serialize_external_count(&spend_sigs, &mut writer)?;

        // vOutputProofsSapling
        zcash_serialize_external_count(&output_proofs, &mut writer)?;

        // bindingSigSapling
        writer.write_all(&<[u8; 64]>::from(self.binding_sig)[..])?;

        Ok(())
    }
}

// we can't split ShieldedData out of Option<ShieldedData> deserialization,
// because the counts are read along with the arrays.
impl ZcashDeserialize for Option<sapling::ShieldedData<SharedAnchor>> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // nSpendsSapling and vSpendsSapling
        let spend_prefixes: Vec<_> = (&mut reader).zcash_deserialize_into()?;

        // nOutputsSapling and vOutputsSapling
        let output_prefixes: Vec<_> = (&mut reader).zcash_deserialize_into()?;

        // nSpendsSapling and nOutputsSapling as variables
        let spends_count = spend_prefixes.len();
        let outputs_count = output_prefixes.len();

        // All the other fields depend on having spends or outputs
        if spend_prefixes.is_empty() && output_prefixes.is_empty() {
            return Ok(None);
        }

        // valueBalanceSapling
        let value_balance = (&mut reader).zcash_deserialize_into()?;

        // anchorSapling
        let shared_anchor = if spends_count > 0 {
            Some(reader.read_32_bytes()?.into())
        } else {
            None
        };

        // vSpendProofsSapling
        let spend_proofs = zcash_deserialize_external_count(spends_count, &mut reader)?;
        // vSpendAuthSigsSapling
        let spend_sigs = zcash_deserialize_external_count(spends_count, &mut reader)?;

        // vOutputProofsSapling
        let output_proofs = zcash_deserialize_external_count(outputs_count, &mut reader)?;

        // bindingSigSapling
        let binding_sig = reader.read_64_bytes()?.into();

        // Create shielded spends from deserialized parts
        let spends: Vec<_> = spend_prefixes
            .into_iter()
            .zip(spend_proofs.into_iter())
            .zip(spend_sigs.into_iter())
            .map(|((prefix, proof), sig)| Spend::<SharedAnchor>::from_v5_parts(prefix, proof, sig))
            .collect();

        // Create shielded outputs from deserialized parts
        let outputs = output_prefixes
            .into_iter()
            .zip(output_proofs.into_iter())
            .map(|(prefix, proof)| Output::from_v5_parts(prefix, proof))
            .collect();

        // Create transfers
        let transfers = match shared_anchor {
            Some(shared_anchor) => sapling::TransferData::SpendsAndMaybeOutputs {
                shared_anchor,
                spends: spends
                    .try_into()
                    .expect("checked spends when parsing shared anchor"),
                maybe_outputs: outputs,
            },
            None => sapling::TransferData::JustOutputs {
                outputs: outputs
                    .try_into()
                    .expect("checked spends or outputs and returned early"),
            },
        };

        Ok(Some(sapling::ShieldedData {
            value_balance,
            transfers,
            binding_sig,
        }))
    }
}

impl ZcashSerialize for Option<orchard::ShieldedData> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            None => {
                // nActionsOrchard
                writer.write_compactsize(0)?;
                // We don't need to write anything else here.
                // "The fields flagsOrchard, valueBalanceOrchard, anchorOrchard, sizeProofsOrchard,
                // proofsOrchard , and bindingSigOrchard are present if and only if nActionsOrchard > 0."
                // https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus notes of the second
                // table, section sign.
            }
            Some(orchard_shielded_data) => {
                orchard_shielded_data.zcash_serialize(&mut writer)?;
            }
        }
        Ok(())
    }
}

impl ZcashSerialize for orchard::ShieldedData {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // Split the AuthorizedAction
        let (actions, sigs): (Vec<orchard::Action>, Vec<Signature<SpendAuth>>) = self
            .actions
            .iter()
            .cloned()
            .map(orchard::AuthorizedAction::into_parts)
            .unzip();

        // nActionsOrchard and vActionsOrchard
        actions.zcash_serialize(&mut writer)?;

        // flagsOrchard
        self.flags.zcash_serialize(&mut writer)?;

        // valueBalanceOrchard
        self.value_balance.zcash_serialize(&mut writer)?;

        // anchorOrchard
        self.shared_anchor.zcash_serialize(&mut writer)?;

        // sizeProofsOrchard and proofsOrchard
        self.proof.zcash_serialize(&mut writer)?;

        // vSpendAuthSigsOrchard
        zcash_serialize_external_count(&sigs, &mut writer)?;

        // bindingSigOrchard
        self.binding_sig.zcash_serialize(&mut writer)?;

        Ok(())
    }
}

// we can't split ShieldedData out of Option<ShieldedData> deserialization,
// because the counts are read along with the arrays.
impl ZcashDeserialize for Option<orchard::ShieldedData> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // nActionsOrchard and vActionsOrchard
        let actions: Vec<orchard::Action> = (&mut reader).zcash_deserialize_into()?;

        // "sizeProofsOrchard ... [is] present if and only if nActionsOrchard > 0"
        // https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
        if actions.is_empty() {
            return Ok(None);
        }

        // flagsOrchard
        let flags: orchard::Flags = (&mut reader).zcash_deserialize_into()?;

        // valueBalanceOrchard
        let value_balance: amount::Amount = (&mut reader).zcash_deserialize_into()?;

        // anchorOrchard
        let shared_anchor: orchard::tree::Root = (&mut reader).zcash_deserialize_into()?;

        // sizeProofsOrchard and proofsOrchard
        let proof: Halo2Proof = (&mut reader).zcash_deserialize_into()?;

        // vSpendAuthSigsOrchard
        let sigs: Vec<Signature<SpendAuth>> =
            zcash_deserialize_external_count(actions.len(), &mut reader)?;

        // bindingSigOrchard
        let binding_sig: Signature<Binding> = (&mut reader).zcash_deserialize_into()?;

        // Create the AuthorizedAction from deserialized parts
        let authorized_actions: Vec<orchard::AuthorizedAction> = actions
            .into_iter()
            .zip(sigs.into_iter())
            .map(|(action, spend_auth_sig)| {
                orchard::AuthorizedAction::from_parts(action, spend_auth_sig)
            })
            .collect();

        let actions: AtLeastOne<orchard::AuthorizedAction> = authorized_actions.try_into()?;

        Ok(Some(orchard::ShieldedData {
            flags,
            value_balance,
            shared_anchor,
            proof,
            actions,
            binding_sig,
        }))
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
                    Some(sapling_shielded_data) => {
                        sapling_shielded_data
                            .value_balance
                            .zcash_serialize(&mut writer)?;
                        writer.write_compactsize(sapling_shielded_data.spends().count() as u64)?;
                        for spend in sapling_shielded_data.spends() {
                            spend.zcash_serialize(&mut writer)?;
                        }
                        writer.write_compactsize(sapling_shielded_data.outputs().count() as u64)?;
                        for output in sapling_shielded_data
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
                network_upgrade,
                lock_time,
                expiry_height,
                inputs,
                outputs,
                sapling_shielded_data,
                orchard_shielded_data,
            } => {
                // Transaction V5 spec:
                // https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus

                // header: Write version 5 and set the fOverwintered bit
                writer.write_u32::<LittleEndian>(5 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(TX_V5_VERSION_GROUP_ID)?;

                // header: Write the nConsensusBranchId
                writer.write_u32::<LittleEndian>(u32::from(
                    network_upgrade
                        .branch_id()
                        .expect("valid transactions must have a network upgrade with a branch id"),
                ))?;

                // transaction validity time and height limits
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;

                // transparent
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;

                // sapling
                sapling_shielded_data.zcash_serialize(&mut writer)?;

                // orchard
                orchard_shielded_data.zcash_serialize(&mut writer)?;
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
                let shielded_spends = Vec::zcash_deserialize(&mut reader)?;
                let shielded_outputs =
                    Vec::<sapling::OutputInTransactionV4>::zcash_deserialize(&mut reader)?
                        .into_iter()
                        .map(Output::from_v4)
                        .collect();

                let joinsplit_data = OptV4Jsd::zcash_deserialize(&mut reader)?;

                let sapling_transfers = if !shielded_spends.is_empty() {
                    Some(sapling::TransferData::SpendsAndMaybeOutputs {
                        shared_anchor: FieldNotPresent,
                        spends: shielded_spends.try_into().expect("checked for spends"),
                        maybe_outputs: shielded_outputs,
                    })
                } else if !shielded_outputs.is_empty() {
                    Some(sapling::TransferData::JustOutputs {
                        outputs: shielded_outputs.try_into().expect("checked for outputs"),
                    })
                } else {
                    None
                };

                let sapling_shielded_data = match sapling_transfers {
                    Some(transfers) => Some(sapling::ShieldedData {
                        value_balance,
                        transfers,
                        binding_sig: reader.read_64_bytes()?.into(),
                    }),
                    None => None,
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
                // header
                let id = reader.read_u32::<LittleEndian>()?;
                if id != TX_V5_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse("expected TX_V5_VERSION_GROUP_ID"));
                }
                // convert the nConsensusBranchId to a NetworkUpgrade
                let network_upgrade = NetworkUpgrade::from_branch_id(
                    reader.read_u32::<LittleEndian>()?,
                )
                .ok_or(SerializationError::Parse(
                    "expected a valid network upgrade from the consensus branch id",
                ))?;

                // transaction validity time and height limits
                let lock_time = LockTime::zcash_deserialize(&mut reader)?;
                let expiry_height = block::Height(reader.read_u32::<LittleEndian>()?);

                // transparent
                let inputs = Vec::zcash_deserialize(&mut reader)?;
                let outputs = Vec::zcash_deserialize(&mut reader)?;

                // sapling
                let sapling_shielded_data = (&mut reader).zcash_deserialize_into()?;

                // orchard
                let orchard_shielded_data = (&mut reader).zcash_deserialize_into()?;

                Ok(Transaction::V5 {
                    network_upgrade,
                    lock_time,
                    expiry_height,
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
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
