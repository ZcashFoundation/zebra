//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use std::{borrow::Borrow, io, sync::Arc};

use halo2::pasta::{group::ff::PrimeField, pallas};
use hex::FromHex;
use reddsa::{orchard::Binding, orchard::SpendAuth, Signature};

use crate::{
    amount,
    block::MAX_BLOCK_BYTES,
    primitives::{Halo2Proof, ZkSnarkProof},
    serialization::{
        zcash_deserialize_external_count, zcash_serialize_empty_list,
        zcash_serialize_external_count, AtLeastOne, ReadZcashExt, SerializationError,
        TrustedPreallocate, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
    },
};

use super::*;
use crate::{orchard, sapling, sprout};

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
        let possible_scalar = pallas::Scalar::from_repr(reader.read_32_bytes()?);

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
        let possible_field_element = pallas::Base::from_repr(reader.read_32_bytes()?);

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
        // Denoted as `nJoinSplit` and `vJoinSplit` in the spec.
        let joinsplits: Vec<_> = self.joinsplits().cloned().collect();
        joinsplits.zcash_serialize(&mut writer)?;

        // Denoted as `joinSplitPubKey` in the spec.
        writer.write_all(&<[u8; 32]>::from(self.pub_key)[..])?;

        // Denoted as `joinSplitSig` in the spec.
        writer.write_all(&<[u8; 64]>::from(self.sig)[..])?;
        Ok(())
    }
}

impl<P> ZcashDeserialize for Option<JoinSplitData<P>>
where
    P: ZkSnarkProof,
    sprout::JoinSplit<P>: TrustedPreallocate,
{
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // Denoted as `nJoinSplit` and `vJoinSplit` in the spec.
        let joinsplits: Vec<sprout::JoinSplit<P>> = (&mut reader).zcash_deserialize_into()?;
        match joinsplits.split_first() {
            None => Ok(None),
            Some((first, rest)) => {
                // Denoted as `joinSplitPubKey` in the spec.
                let pub_key = reader.read_32_bytes()?.into();
                // Denoted as `joinSplitSig` in the spec.
                let sig = reader.read_64_bytes()?.into();
                Ok(Some(JoinSplitData {
                    first: first.clone(),
                    rest: rest.to_vec(),
                    pub_key,
                    sig,
                }))
            }
        }
    }
}

// Transaction::V4 sapling ShieldedData serialization.
// Used in property-based tests to roundtrip ShieldedData<PerSpendAnchor> directly.

impl ZcashSerialize for Option<sapling::ShieldedData<sapling::PerSpendAnchor>> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            None => {
                zcash_serialize_empty_list(&mut writer)?;
                zcash_serialize_empty_list(&mut writer)?;
            }
            Some(sd) => {
                sd.zcash_serialize(&mut writer)?;
            }
        }
        Ok(())
    }
}

impl ZcashSerialize for sapling::ShieldedData<sapling::PerSpendAnchor> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let spends: Vec<_> = self.spends().cloned().collect();
        let outputs: Vec<_> = self
            .outputs()
            .cloned()
            .map(sapling::Output::into_v4)
            .collect();

        spends.zcash_serialize(&mut writer)?;
        outputs.zcash_serialize(&mut writer)?;
        self.value_balance.zcash_serialize(&mut writer)?;
        writer.write_all(&<[u8; 64]>::from(self.binding_sig)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Option<sapling::ShieldedData<sapling::PerSpendAnchor>> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let spends: Vec<sapling::Spend<sapling::PerSpendAnchor>> =
            (&mut reader).zcash_deserialize_into()?;
        let outputs: Vec<sapling::OutputInTransactionV4> =
            (&mut reader).zcash_deserialize_into()?;

        if spends.is_empty() && outputs.is_empty() {
            return Ok(None);
        }

        let value_balance = (&mut reader).zcash_deserialize_into()?;
        let binding_sig = reader.read_64_bytes()?.into();

        let outputs: Vec<sapling::Output> = outputs
            .into_iter()
            .map(sapling::OutputInTransactionV4::into_output)
            .collect();

        let transfers = match spends.split_first() {
            None => sapling::TransferData::JustOutputs {
                outputs: outputs.try_into().map_err(|_| {
                    SerializationError::Parse(
                        "ShieldedData<PerSpendAnchor> with no spends or outputs",
                    )
                })?,
            },
            Some((first, rest)) => sapling::TransferData::SpendsAndMaybeOutputs {
                shared_anchor: sapling::FieldNotPresent,
                spends: std::iter::once(first.clone())
                    .chain(rest.iter().cloned())
                    .collect::<Vec<_>>()
                    .try_into()
                    .map_err(|_| {
                        SerializationError::Parse("ShieldedData<PerSpendAnchor> spend list empty")
                    })?,
                maybe_outputs: outputs,
            },
        };

        Ok(Some(sapling::ShieldedData {
            value_balance,
            transfers,
            binding_sig,
        }))
    }
}

// Transaction::V5 serializes sapling ShieldedData in a single continuous byte
// range, so we can implement its serialization and deserialization separately.
// (Unlike V4, where it must be serialized as part of the transaction.)

impl ZcashSerialize for Option<sapling::ShieldedData<sapling::SharedAnchor>> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            None => {
                // Denoted as `nSpendsSapling` in the spec.
                zcash_serialize_empty_list(&mut writer)?;
                // Denoted as `nOutputsSapling` in the spec.
                zcash_serialize_empty_list(&mut writer)?;
            }
            Some(sapling_shielded_data) => {
                sapling_shielded_data.zcash_serialize(&mut writer)?;
            }
        }
        Ok(())
    }
}

impl ZcashSerialize for sapling::ShieldedData<sapling::SharedAnchor> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // Collect arrays for Spends
        // There's no unzip3, so we have to unzip twice.
        let (spend_prefixes, spend_proofs_sigs): (Vec<_>, Vec<_>) = self
            .spends()
            .cloned()
            .map(sapling::Spend::<sapling::SharedAnchor>::into_v5_parts)
            .map(|(prefix, proof, sig)| (prefix, (proof, sig)))
            .unzip();
        let (spend_proofs, spend_sigs) = spend_proofs_sigs.into_iter().unzip();

        // Collect arrays for Outputs
        let (output_prefixes, output_proofs): (Vec<_>, _) = self
            .outputs()
            .cloned()
            .map(sapling::Output::into_v5_parts)
            .unzip();

        // Denoted as `nSpendsSapling` and `vSpendsSapling` in the spec.
        spend_prefixes.zcash_serialize(&mut writer)?;
        // Denoted as `nOutputsSapling` and `vOutputsSapling` in the spec.
        output_prefixes.zcash_serialize(&mut writer)?;

        // Denoted as `valueBalanceSapling` in the spec.
        self.value_balance.zcash_serialize(&mut writer)?;

        // Denoted as `anchorSapling` in the spec.
        // `TransferData` ensures this field is only present when there is at
        // least one spend.
        if let Some(shared_anchor) = self.shared_anchor() {
            writer.write_all(&<[u8; 32]>::from(shared_anchor)[..])?;
        }

        // Denoted as `vSpendProofsSapling` in the spec.
        zcash_serialize_external_count(&spend_proofs, &mut writer)?;
        // Denoted as `vSpendAuthSigsSapling` in the spec.
        zcash_serialize_external_count(&spend_sigs, &mut writer)?;

        // Denoted as `vOutputProofsSapling` in the spec.
        zcash_serialize_external_count(&output_proofs, &mut writer)?;

        // Denoted as `bindingSigSapling` in the spec.
        writer.write_all(&<[u8; 64]>::from(self.binding_sig)[..])?;

        Ok(())
    }
}

// we can't split ShieldedData out of Option<ShieldedData> deserialization,
// because the counts are read along with the arrays.
impl ZcashDeserialize for Option<sapling::ShieldedData<sapling::SharedAnchor>> {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // Denoted as `nSpendsSapling` and `vSpendsSapling` in the spec.
        let spend_prefixes: Vec<_> = (&mut reader).zcash_deserialize_into()?;

        // Denoted as `nOutputsSapling` and `vOutputsSapling` in the spec.
        let output_prefixes: Vec<_> = (&mut reader).zcash_deserialize_into()?;

        // nSpendsSapling and nOutputsSapling as variables
        let spends_count = spend_prefixes.len();
        let outputs_count = output_prefixes.len();

        // All the other fields depend on having spends or outputs
        if spend_prefixes.is_empty() && output_prefixes.is_empty() {
            return Ok(None);
        }

        // Denoted as `valueBalanceSapling` in the spec.
        let value_balance = (&mut reader).zcash_deserialize_into()?;

        // Denoted as `anchorSapling` in the spec.
        //
        // # Consensus
        //
        // > Elements of a Spend description MUST be valid encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#spenddesc
        //
        // Type is `B^{[ℓ_{Sapling}_{Merkle}]}`, i.e. 32 bytes
        //
        // > LEOS2IP_{256}(anchorSapling), if present, MUST be less than 𝑞_𝕁.
        //
        // https://zips.z.cash/protocol/protocol.pdf#spendencodingandconsensus
        //
        // Validated in [`crate::sapling::tree::Root::zcash_deserialize`].
        let shared_anchor = if spends_count > 0 {
            Some((&mut reader).zcash_deserialize_into()?)
        } else {
            None
        };

        // Denoted as `vSpendProofsSapling` in the spec.
        //
        // # Consensus
        //
        // > Elements of a Spend description MUST be valid encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#spenddesc
        //
        // Type is `ZKSpend.Proof`, described in
        // https://zips.z.cash/protocol/protocol.pdf#grothencoding
        // It is not enforced here; this just reads 192 bytes.
        // The type is validated when validating the proof, see
        // [`groth16::Item::try_from`]. In #3179 we plan to validate here instead.
        let spend_proofs = zcash_deserialize_external_count(spends_count, &mut reader)?;

        // Denoted as `vSpendAuthSigsSapling` in the spec.
        //
        // # Consensus
        //
        // > Elements of a Spend description MUST be valid encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#spenddesc
        //
        // Type is SpendAuthSig^{Sapling}.Signature, i.e.
        // B^Y^{[ceiling(ℓ_G/8) + ceiling(bitlength(𝑟_G)/8)]} i.e. 64 bytes
        // https://zips.z.cash/protocol/protocol.pdf#concretereddsa
        // See [`redjubjub::Signature<SpendAuth>::zcash_deserialize`].
        let spend_sigs = zcash_deserialize_external_count(spends_count, &mut reader)?;

        // Denoted as `vOutputProofsSapling` in the spec.
        //
        // # Consensus
        //
        // > Elements of an Output description MUST be valid encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#outputdesc
        //
        // Type is `ZKOutput.Proof`, described in
        // https://zips.z.cash/protocol/protocol.pdf#grothencoding
        // It is not enforced here; this just reads 192 bytes.
        // The type is validated when validating the proof, see
        // [`groth16::Item::try_from`]. In #3179 we plan to validate here instead.
        let output_proofs = zcash_deserialize_external_count(outputs_count, &mut reader)?;

        // Denoted as `bindingSigSapling` in the spec.
        let binding_sig = reader.read_64_bytes()?.into();

        // Create shielded spends from deserialized parts
        let spends: Vec<_> = spend_prefixes
            .into_iter()
            .zip(spend_proofs)
            .zip(spend_sigs)
            .map(|((prefix, proof), sig)| {
                sapling::Spend::<sapling::SharedAnchor>::from_v5_parts(prefix, proof, sig)
            })
            .collect();

        // Create shielded outputs from deserialized parts
        let outputs = output_prefixes
            .into_iter()
            .zip(output_proofs)
            .map(|(prefix, proof)| sapling::Output::from_v5_parts(prefix, proof))
            .collect();

        // Create transfers
        //
        // # Consensus
        //
        // > The anchor of each Spend description MUST refer to some earlier
        // > block’s final Sapling treestate. The anchor is encoded separately
        // > in each Spend description for v4 transactions, or encoded once and
        // > shared between all Spend descriptions in a v5 transaction.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#spendsandoutputs>
        //
        // This rule is also implemented in
        // [`zebra_state::service::check::anchor`] and
        // [`zebra_chain::sapling::spend`].
        //
        // The "anchor encoding for v5 transactions" is implemented here.
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
                // Denoted as `nActionsOrchard` in the spec.
                zcash_serialize_empty_list(writer)?;

                // We don't need to write anything else here.
                // "The fields flagsOrchard, valueBalanceOrchard, anchorOrchard, sizeProofsOrchard,
                // proofsOrchard , and bindingSigOrchard are present if and only if nActionsOrchard > 0."
                // `§` note of the second table of https://zips.z.cash/protocol/protocol.pdf#txnencoding
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

        // Denoted as `nActionsOrchard` and `vActionsOrchard` in the spec.
        actions.zcash_serialize(&mut writer)?;

        // Denoted as `flagsOrchard` in the spec.
        self.flags.zcash_serialize(&mut writer)?;

        // Denoted as `valueBalanceOrchard` in the spec.
        self.value_balance.zcash_serialize(&mut writer)?;

        // Denoted as `anchorOrchard` in the spec.
        self.shared_anchor.zcash_serialize(&mut writer)?;

        // Denoted as `sizeProofsOrchard` and `proofsOrchard` in the spec.
        self.proof.zcash_serialize(&mut writer)?;

        // Denoted as `vSpendAuthSigsOrchard` in the spec.
        zcash_serialize_external_count(&sigs, &mut writer)?;

        // Denoted as `bindingSigOrchard` in the spec.
        self.binding_sig.zcash_serialize(&mut writer)?;

        Ok(())
    }
}

// we can't split ShieldedData out of Option<ShieldedData> deserialization,
// because the counts are read along with the arrays.
impl ZcashDeserialize for Option<orchard::ShieldedData> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // Denoted as `nActionsOrchard` and `vActionsOrchard` in the spec.
        let actions: Vec<orchard::Action> = (&mut reader).zcash_deserialize_into()?;

        // "The fields flagsOrchard, valueBalanceOrchard, anchorOrchard, sizeProofsOrchard,
        // proofsOrchard , and bindingSigOrchard are present if and only if nActionsOrchard > 0."
        // `§` note of the second table of https://zips.z.cash/protocol/protocol.pdf#txnencoding
        if actions.is_empty() {
            return Ok(None);
        }

        // # Consensus
        //
        // > Elements of an Action description MUST be canonical encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#actiondesc
        //
        // Some Action elements are validated in this function; they are described below.

        // Denoted as `flagsOrchard` in the spec.
        // Consensus: type of each flag is 𝔹, i.e. a bit. This is enforced implicitly
        // in [`Flags::zcash_deserialized`].
        let flags: orchard::Flags = (&mut reader).zcash_deserialize_into()?;

        // Denoted as `valueBalanceOrchard` in the spec.
        let value_balance: amount::Amount = (&mut reader).zcash_deserialize_into()?;

        // Denoted as `anchorOrchard` in the spec.
        // Consensus: type is `{0 .. 𝑞_ℙ − 1}`. See [`orchard::tree::Root::zcash_deserialize`].
        let shared_anchor: orchard::tree::Root = (&mut reader).zcash_deserialize_into()?;

        // Denoted as `sizeProofsOrchard` and `proofsOrchard` in the spec.
        // Consensus: type is `ZKAction.Proof`, i.e. a byte sequence.
        // https://zips.z.cash/protocol/protocol.pdf#halo2encoding
        let proof: Halo2Proof = (&mut reader).zcash_deserialize_into()?;

        // Denoted as `vSpendAuthSigsOrchard` in the spec.
        // Consensus: this validates the `spendAuthSig` elements, whose type is
        // SpendAuthSig^{Orchard}.Signature, i.e.
        // B^Y^{[ceiling(ℓ_G/8) + ceiling(bitlength(𝑟_G)/8)]} i.e. 64 bytes
        // See [`Signature::zcash_deserialize`].
        let sigs: Vec<Signature<SpendAuth>> =
            zcash_deserialize_external_count(actions.len(), &mut reader)?;

        // Denoted as `bindingSigOrchard` in the spec.
        let binding_sig: Signature<Binding> = (&mut reader).zcash_deserialize_into()?;

        // Create the AuthorizedAction from deserialized parts
        let authorized_actions: Vec<orchard::AuthorizedAction> = actions
            .into_iter()
            .zip(sigs)
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

impl<T: reddsa::SigType> ZcashSerialize for reddsa::Signature<T> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 64]>::from(*self)[..])?;
        Ok(())
    }
}

impl<T: reddsa::SigType> ZcashDeserialize for reddsa::Signature<T> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_64_bytes()?.into())
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
/// and a signature script, which always takes a min of 1 byte (for a length 0 script).
pub(crate) const MIN_TRANSPARENT_INPUT_SIZE: u64 = 32 + 4 + 4 + 1;

/// A Transparent output has an 8 byte value and script which takes a min of 1 byte.
pub(crate) const MIN_TRANSPARENT_OUTPUT_SIZE: u64 = 8 + 1;

/// All txs must have at least one input, a 4 byte locktime, and at least one output.
///
/// Shielded transfers are much larger than transparent transfers,
/// so this is the minimum transaction size.
pub const MIN_TRANSPARENT_TX_SIZE: u64 =
    MIN_TRANSPARENT_INPUT_SIZE + 4 + MIN_TRANSPARENT_OUTPUT_SIZE;

/// The minimum transaction size for v4 transactions.
///
/// v4 transactions also have an expiry height.
pub const MIN_TRANSPARENT_TX_V4_SIZE: u64 = MIN_TRANSPARENT_TX_SIZE + 4;

/// The minimum transaction size for v5 transactions.
///
/// v5 transactions also have an expiry height and a consensus branch ID.
pub const MIN_TRANSPARENT_TX_V5_SIZE: u64 = MIN_TRANSPARENT_TX_SIZE + 4 + 4;

/// No valid Zcash message contains more transactions than can fit in a single block
///
/// `tx` messages contain a single transaction, and `block` messages are limited to the maximum
/// block size.
impl TrustedPreallocate for Transaction {
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

/// A serialized transaction.
///
/// Stores bytes that are guaranteed to be deserializable into a [`Transaction`].
///
/// Sorts in lexicographic order of the transaction's serialized data.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SerializedTransaction {
    bytes: Vec<u8>,
}

impl fmt::Display for SerializedTransaction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&hex::encode(&self.bytes))
    }
}

impl fmt::Debug for SerializedTransaction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // A transaction with a lot of transfers can be extremely long in logs.
        let mut data_truncated = hex::encode(&self.bytes);
        if data_truncated.len() > 1003 {
            let end = data_truncated.len() - 500;
            // Replace the middle bytes with "...", but leave 500 bytes on either side.
            // The data is hex, so this replacement won't panic.
            data_truncated.replace_range(500..=end, "...");
        }

        f.debug_tuple("SerializedTransaction")
            .field(&data_truncated)
            .finish()
    }
}

/// Build a [`SerializedTransaction`] by serializing a block.
impl<B: Borrow<Transaction>> From<B> for SerializedTransaction {
    fn from(tx: B) -> Self {
        SerializedTransaction {
            bytes: tx
                .borrow()
                .zcash_serialize_to_vec()
                .expect("Writing to a `Vec` should never fail"),
        }
    }
}

/// Access the serialized bytes of a [`SerializedTransaction`].
impl AsRef<[u8]> for SerializedTransaction {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl From<Vec<u8>> for SerializedTransaction {
    fn from(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl FromHex for SerializedTransaction {
    type Error = <Vec<u8> as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let bytes = <Vec<u8>>::from_hex(hex)?;

        Ok(bytes.into())
    }
}
