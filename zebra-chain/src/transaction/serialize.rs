//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use std::{borrow::Borrow, io, sync::Arc};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use halo2::pasta::group::ff::PrimeField;
use hex::FromHex;
use reddsa::{orchard::Binding, orchard::SpendAuth, Signature};

use crate::{
    amount,
    block::MAX_BLOCK_BYTES,
    orchard::{ActionGroup, OrchardVanilla, OrchardZSA, ShieldedDataFlavor},
    parameters::{OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID, TX_V5_VERSION_GROUP_ID},
    primitives::{Halo2Proof, ZkSnarkProof},
    serialization::{
        zcash_deserialize_external_count, zcash_serialize_empty_list,
        zcash_serialize_external_count, AtLeastOne, CompactSizeMessage, ReadZcashExt,
        SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashDeserializeInto,
        ZcashSerialize,
    },
};

#[cfg(feature = "tx-v6")]
use crate::parameters::TX_V6_VERSION_GROUP_ID;

use super::*;
use crate::sapling;

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

impl<FL: ShieldedDataFlavor> ZcashSerialize for Option<orchard::ShieldedData<FL>>
where
    orchard::ShieldedData<FL>: ZcashSerialize,
{
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

impl ZcashSerialize for orchard::ShieldedData<OrchardVanilla> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        assert!(
            self.action_groups.len() == 1,
            "V6 transaction must contain exactly one action group"
        );

        let action_group = self.action_groups.first();

        // Split the AuthorizedAction
        let (actions, sigs): (
            Vec<orchard::Action<OrchardVanilla>>,
            Vec<Signature<SpendAuth>>,
        ) = action_group
            .actions
            .iter()
            .cloned()
            .map(orchard::AuthorizedAction::into_parts)
            .unzip();

        // Denoted as `nActionsOrchard` and `vActionsOrchard` in the spec.
        actions.zcash_serialize(&mut writer)?;

        // Denoted as `flagsOrchard` in the spec.
        action_group.flags.zcash_serialize(&mut writer)?;

        // Denoted as `valueBalanceOrchard` in the spec.
        self.value_balance.zcash_serialize(&mut writer)?;

        // Denoted as `anchorOrchard` in the spec.
        action_group.shared_anchor.zcash_serialize(&mut writer)?;

        // Denoted as `sizeProofsOrchard` and `proofsOrchard` in the spec.
        action_group.proof.zcash_serialize(&mut writer)?;

        // Denoted as `vSpendAuthSigsOrchard` in the spec.
        zcash_serialize_external_count(&sigs, &mut writer)?;

        // Denoted as `bindingSigOrchard` in the spec.
        self.binding_sig.zcash_serialize(&mut writer)?;

        Ok(())
    }
}

// FIXME: Try to avoid duplication with OrchardVanilla version
#[cfg(feature = "tx-v6")]
#[allow(clippy::unwrap_in_result)]
impl ZcashSerialize for orchard::ShieldedData<OrchardZSA> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        #[cfg(not(feature = "zsa-swap"))]
        assert!(
            self.action_groups.len() == 1,
            "V6 transaction must contain exactly one action group"
        );

        // FIXME: consider using use zcash_serialize_external_count for or Vec::zcash_serialize
        for action_group in self.action_groups.iter() {
            // Denoted as `nActionGroupsOrchard` in the spec  (ZIP 230) (must be one for V6/NU7).
            CompactSizeMessage::try_from(self.action_groups.len())
                .expect("nActionGroupsOrchard should convert to CompactSizeMessage")
                .zcash_serialize(&mut writer)?;

            // Split the AuthorizedAction
            let (actions, sigs): (Vec<orchard::Action<OrchardZSA>>, Vec<Signature<SpendAuth>>) =
                action_group
                    .actions
                    .iter()
                    .cloned()
                    .map(orchard::AuthorizedAction::into_parts)
                    .unzip();

            // Denoted as `nActionsOrchard` and `vActionsOrchard` in the spec.
            actions.zcash_serialize(&mut writer)?;

            // Denoted as `flagsOrchard` in the spec.
            action_group.flags.zcash_serialize(&mut writer)?;

            // Denoted as `anchorOrchard` in the spec.
            action_group.shared_anchor.zcash_serialize(&mut writer)?;

            // Denoted as `sizeProofsOrchard` and `proofsOrchard` in the spec.
            action_group.proof.zcash_serialize(&mut writer)?;

            // Denoted as `nAGExpiryHeight` in the spec  (ZIP 230) (must be zero for V6/NU7).
            writer.write_u32::<LittleEndian>(0)?;

            // Denoted as `vAssetBurn` in the spec (ZIP 230).
            #[cfg(feature = "zsa-swap")]
            action_group.burn.zcash_serialize(&mut writer)?;

            // Denoted as `vSpendAuthSigsOrchard` in the spec.
            zcash_serialize_external_count(&sigs, &mut writer)?;
        }

        // Denoted as `valueBalanceOrchard` in the spec.
        self.value_balance.zcash_serialize(&mut writer)?;

        // Denoted as `vAssetBurn` in the spec (ZIP 230).
        #[cfg(not(feature = "zsa-swap"))]
        self.action_groups
            .first()
            .burn
            .zcash_serialize(&mut writer)?;

        // Denoted as `bindingSigOrchard` in the spec.
        self.binding_sig.zcash_serialize(&mut writer)?;

        Ok(())
    }
}

// we can't split ShieldedData out of Option<ShieldedData> deserialization,
// because the counts are read along with the arrays.
impl ZcashDeserialize for Option<orchard::ShieldedData<OrchardVanilla>> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // Denoted as `nActionsOrchard` and `vActionsOrchard` in the spec.
        let actions: Vec<orchard::Action<OrchardVanilla>> =
            (&mut reader).zcash_deserialize_into()?;

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
        let authorized_actions: Vec<orchard::AuthorizedAction<OrchardVanilla>> = actions
            .into_iter()
            .zip(sigs)
            .map(|(action, spend_auth_sig)| {
                orchard::AuthorizedAction::from_parts(action, spend_auth_sig)
            })
            .collect();

        let actions: AtLeastOne<orchard::AuthorizedAction<OrchardVanilla>> =
            authorized_actions.try_into()?;

        Ok(Some(orchard::ShieldedData::<OrchardVanilla> {
            action_groups: AtLeastOne::from_one(ActionGroup {
                flags,
                shared_anchor,
                proof,
                actions,
                burn: Default::default(),
            }),
            value_balance,
            binding_sig,
        }))
    }
}

// FIXME: Try to avoid duplication with OrchardVanilla version
// we can't split ShieldedData out of Option<ShieldedData> deserialization,
// because the counts are read along with the arrays.
#[cfg(feature = "tx-v6")]
impl ZcashDeserialize for Option<orchard::ShieldedData<OrchardZSA>> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // FIXME: Implement deserialization of multiple action groups (under a feature flag)

        // Denoted as `nActionGroupsOrchard` in the spec (ZIP 230) (must be one for V6/NU7).
        let n_action_groups: usize = (&mut reader)
            .zcash_deserialize_into::<CompactSizeMessage>()?
            .into();

        if n_action_groups == 0 {
            return Ok(None);
        }

        #[cfg(not(feature = "zsa-swap"))]
        if n_action_groups != 1 {
            return Err(SerializationError::Parse(
                "V6 transaction must contain exactly one action group",
            ));
        }

        let mut action_groups = Vec::with_capacity(n_action_groups);

        // FIXME: use zcash_deserialize_external_count for or Vec::zcash_deserialize for allocation safety
        for _ in 0..n_action_groups {
            // Denoted as `nActionsOrchard` and `vActionsOrchard` in the spec.
            let actions: Vec<orchard::Action<OrchardZSA>> =
                (&mut reader).zcash_deserialize_into()?;

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

            // Denoted as `anchorOrchard` in the spec.
            // Consensus: type is `{0 .. 𝑞_ℙ − 1}`. See [`orchard::tree::Root::zcash_deserialize`].
            let shared_anchor: orchard::tree::Root = (&mut reader).zcash_deserialize_into()?;

            // Denoted as `sizeProofsOrchard` and `proofsOrchard` in the spec.
            // Consensus: type is `ZKAction.Proof`, i.e. a byte sequence.
            // https://zips.z.cash/protocol/protocol.pdf#halo2encoding
            let proof: Halo2Proof = (&mut reader).zcash_deserialize_into()?;

            // Denoted as `nAGExpiryHeight` in the spec  (ZIP 230) (must be zero for V6/NU7).
            let n_ag_expiry_height = reader.read_u32::<LittleEndian>()?;
            if n_ag_expiry_height != 0 {
                return Err(SerializationError::Parse("nAGExpiryHeight for V6/NU7"));
            }

            // Denoted as `vAssetBurn` in the spec  (ZIP 230).
            #[cfg(feature = "zsa-swap")]
            let burn = (&mut reader).zcash_deserialize_into()?;
            #[cfg(not(feature = "zsa-swap"))]
            let burn = Default::default();

            // Denoted as `vSpendAuthSigsOrchard` in the spec.
            // Consensus: this validates the `spendAuthSig` elements, whose type is
            // SpendAuthSig^{Orchard}.Signature, i.e.
            // B^Y^{[ceiling(ℓ_G/8) + ceiling(bitlength(𝑟_G)/8)]} i.e. 64 bytes
            // See [`Signature::zcash_deserialize`].
            let sigs: Vec<Signature<SpendAuth>> =
                zcash_deserialize_external_count(actions.len(), &mut reader)?;

            // Create the AuthorizedAction from deserialized parts
            let authorized_actions: Vec<orchard::AuthorizedAction<OrchardZSA>> = actions
                .into_iter()
                .zip(sigs)
                .map(|(action, spend_auth_sig)| {
                    orchard::AuthorizedAction::from_parts(action, spend_auth_sig)
                })
                .collect();

            let actions: AtLeastOne<orchard::AuthorizedAction<OrchardZSA>> =
                authorized_actions.try_into()?;

            action_groups.push(ActionGroup {
                flags,
                shared_anchor,
                proof,
                actions,
                burn,
            })
        }

        // Denoted as `valueBalanceOrchard` in the spec.
        let value_balance: amount::Amount = (&mut reader).zcash_deserialize_into()?;

        // Denoted as `vAssetBurn` in the spec  (ZIP 230).
        #[cfg(not(feature = "zsa-swap"))]
        {
            action_groups[0].burn = (&mut reader).zcash_deserialize_into()?;
        }

        // Denoted as `bindingSigOrchard` in the spec.
        let binding_sig: Signature<Binding> = (&mut reader).zcash_deserialize_into()?;

        Ok(Some(orchard::ShieldedData::<OrchardZSA> {
            action_groups: action_groups.try_into()?,
            value_balance,
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

impl ZcashSerialize for Transaction {
    #[allow(clippy::unwrap_in_result)]
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

        // Denoted as `header` in the spec, contains the `fOverwintered` flag and the `version` field.
        // Write `version` and set the `fOverwintered` bit if necessary
        let overwintered_flag = if self.is_overwintered() { 1 << 31 } else { 0 };
        let version = overwintered_flag | self.version();

        writer.write_u32::<LittleEndian>(version)?;

        match self {
            Transaction::V1 {
                inputs,
                outputs,
                lock_time,
            } => {
                // Denoted as `tx_in_count` and `tx_in` in the spec.
                inputs.zcash_serialize(&mut writer)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                outputs.zcash_serialize(&mut writer)?;

                // Denoted as `lock_time` in the spec.
                lock_time.zcash_serialize(&mut writer)?;
            }
            Transaction::V2 {
                inputs,
                outputs,
                lock_time,
                joinsplit_data,
            } => {
                // Denoted as `tx_in_count` and `tx_in` in the spec.
                inputs.zcash_serialize(&mut writer)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                outputs.zcash_serialize(&mut writer)?;

                // Denoted as `lock_time` in the spec.
                lock_time.zcash_serialize(&mut writer)?;

                // A bundle of fields denoted in the spec as `nJoinSplit`, `vJoinSplit`,
                // `joinSplitPubKey` and `joinSplitSig`.
                match joinsplit_data {
                    // Write 0 for nJoinSplits to signal no JoinSplitData.
                    None => zcash_serialize_empty_list(writer)?,
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
                // Denoted as `nVersionGroupId` in the spec.
                writer.write_u32::<LittleEndian>(OVERWINTER_VERSION_GROUP_ID)?;

                // Denoted as `tx_in_count` and `tx_in` in the spec.
                inputs.zcash_serialize(&mut writer)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                outputs.zcash_serialize(&mut writer)?;

                // Denoted as `lock_time` in the spec.
                lock_time.zcash_serialize(&mut writer)?;

                writer.write_u32::<LittleEndian>(expiry_height.0)?;

                // A bundle of fields denoted in the spec as `nJoinSplit`, `vJoinSplit`,
                // `joinSplitPubKey` and `joinSplitSig`.
                match joinsplit_data {
                    // Write 0 for nJoinSplits to signal no JoinSplitData.
                    None => zcash_serialize_empty_list(writer)?,
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
                // Transaction V4 spec:
                // https://zips.z.cash/protocol/protocol.pdf#txnencoding

                // Denoted as `nVersionGroupId` in the spec.
                writer.write_u32::<LittleEndian>(SAPLING_VERSION_GROUP_ID)?;

                // Denoted as `tx_in_count` and `tx_in` in the spec.
                inputs.zcash_serialize(&mut writer)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                outputs.zcash_serialize(&mut writer)?;

                // Denoted as `lock_time` in the spec.
                lock_time.zcash_serialize(&mut writer)?;

                // Denoted as `nExpiryHeight` in the spec.
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
                        zcash_serialize_empty_list(&mut writer)?;
                        zcash_serialize_empty_list(&mut writer)?;
                    }
                    Some(sapling_shielded_data) => {
                        // Denoted as `valueBalanceSapling` in the spec.
                        sapling_shielded_data
                            .value_balance
                            .zcash_serialize(&mut writer)?;

                        // Denoted as `nSpendsSapling` and `vSpendsSapling` in the spec.
                        let spends: Vec<_> = sapling_shielded_data.spends().cloned().collect();
                        spends.zcash_serialize(&mut writer)?;

                        // Denoted as `nOutputsSapling` and `vOutputsSapling` in the spec.
                        let outputs: Vec<_> = sapling_shielded_data
                            .outputs()
                            .cloned()
                            .map(sapling::OutputInTransactionV4)
                            .collect();
                        outputs.zcash_serialize(&mut writer)?;
                    }
                }

                // A bundle of fields denoted in the spec as `nJoinSplit`, `vJoinSplit`,
                // `joinSplitPubKey` and `joinSplitSig`.
                match joinsplit_data {
                    None => zcash_serialize_empty_list(&mut writer)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }

                // Denoted as `bindingSigSapling` in the spec.
                if let Some(shielded_data) = sapling_shielded_data {
                    writer.write_all(&<[u8; 64]>::from(shielded_data.binding_sig)[..])?;
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
                // https://zips.z.cash/protocol/protocol.pdf#txnencoding

                // Denoted as `nVersionGroupId` in the spec.
                writer.write_u32::<LittleEndian>(TX_V5_VERSION_GROUP_ID)?;

                // Denoted as `nConsensusBranchId` in the spec.
                writer.write_u32::<LittleEndian>(u32::from(
                    network_upgrade
                        .branch_id()
                        .expect("valid transactions must have a network upgrade with a branch id"),
                ))?;

                // Denoted as `lock_time` in the spec.
                lock_time.zcash_serialize(&mut writer)?;

                // Denoted as `nExpiryHeight` in the spec.
                writer.write_u32::<LittleEndian>(expiry_height.0)?;

                // Denoted as `tx_in_count` and `tx_in` in the spec.
                inputs.zcash_serialize(&mut writer)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                outputs.zcash_serialize(&mut writer)?;

                // A bundle of fields denoted in the spec as `nSpendsSapling`, `vSpendsSapling`,
                // `nOutputsSapling`,`vOutputsSapling`, `valueBalanceSapling`, `anchorSapling`,
                // `vSpendProofsSapling`, `vSpendAuthSigsSapling`, `vOutputProofsSapling` and
                // `bindingSigSapling`.
                sapling_shielded_data.zcash_serialize(&mut writer)?;

                // A bundle of fields denoted in the spec as `nActionsOrchard`, `vActionsOrchard`,
                // `flagsOrchard`,`valueBalanceOrchard`, `anchorOrchard`, `sizeProofsOrchard`,
                // `proofsOrchard`, `vSpendAuthSigsOrchard`, and `bindingSigOrchard`.
                orchard_shielded_data.zcash_serialize(&mut writer)?;
            }

            #[cfg(feature = "tx-v6")]
            Transaction::V6 {
                network_upgrade,
                lock_time,
                expiry_height,
                inputs,
                outputs,
                sapling_shielded_data,
                orchard_shielded_data,
                orchard_zsa_issue_data,
            } => {
                // Transaction V6 spec:
                // https://zips.z.cash/zip-0230#transaction-format

                // Denoted as `nVersionGroupId` in the spec.
                writer.write_u32::<LittleEndian>(TX_V6_VERSION_GROUP_ID)?;

                // Denoted as `nConsensusBranchId` in the spec.
                writer.write_u32::<LittleEndian>(u32::from(
                    network_upgrade
                        .branch_id()
                        .expect("valid transactions must have a network upgrade with a branch id"),
                ))?;

                // Denoted as `lock_time` in the spec.
                lock_time.zcash_serialize(&mut writer)?;

                // Denoted as `nExpiryHeight` in the spec.
                writer.write_u32::<LittleEndian>(expiry_height.0)?;

                // Denoted as `tx_in_count` and `tx_in` in the spec.
                inputs.zcash_serialize(&mut writer)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                outputs.zcash_serialize(&mut writer)?;

                // A bundle of fields denoted in the spec as `nSpendsSapling`, `vSpendsSapling`,
                // `nOutputsSapling`,`vOutputsSapling`, `valueBalanceSapling`, `anchorSapling`,
                // `vSpendProofsSapling`, `vSpendAuthSigsSapling`, `vOutputProofsSapling` and
                // `bindingSigSapling`.
                sapling_shielded_data.zcash_serialize(&mut writer)?;

                // A bundle of fields denoted in the spec as `nActionsOrchard`, `vActionsOrchard`,
                // `flagsOrchard`,`valueBalanceOrchard`, `anchorOrchard`, `sizeProofsOrchard`,
                // `proofsOrchard`, `vSpendAuthSigsOrchard`, and `bindingSigOrchard`.
                orchard_shielded_data.zcash_serialize(&mut writer)?;

                // OrchardZSA Issuance Fields.
                orchard_zsa_issue_data.zcash_serialize(&mut writer)?;
            }
        }
        Ok(())
    }
}

impl ZcashDeserialize for Transaction {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        // # Consensus
        //
        // > [Pre-Sapling] The encoded size of the transaction MUST be less than or
        // > equal to 100000 bytes.
        //
        // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
        //
        // Zebra does not verify this rule because we checkpoint up to Canopy blocks, but:
        // Since transactions must get mined into a block to be useful,
        // we reject transactions that are larger than blocks.
        //
        // If the limit is reached, we'll get an UnexpectedEof error.
        let mut limited_reader = reader.take(MAX_BLOCK_BYTES);

        let (version, overwintered) = {
            const LOW_31_BITS: u32 = (1 << 31) - 1;
            // Denoted as `header` in the spec, contains the `fOverwintered` flag and the `version` field.
            let header = limited_reader.read_u32::<LittleEndian>()?;
            (header & LOW_31_BITS, header >> 31 != 0)
        };

        // # Consensus
        //
        // The next rules apply for different transaction versions as follows:
        //
        // [Pre-Overwinter]: Transactions version 1 and 2.
        // [Overwinter onward]: Transactions version 3 and above.
        // [Overwinter only, pre-Sapling]: Transactions version 3.
        // [Sapling to Canopy inclusive, pre-NU5]: Transactions version 4.
        // [NU5 onward]: Transactions version 4 and above.
        //
        // > The transaction version number MUST be greater than or equal to 1.
        //
        // > [Pre-Overwinter] The fOverwintered fag MUST NOT be set.
        //
        // > [Overwinter onward] The version group ID MUST be recognized.
        //
        // > [Overwinter onward] The fOverwintered flag MUST be set.
        //
        // > [Overwinter only, pre-Sapling] The transaction version number MUST be 3,
        // > and the version group ID MUST be 0x03C48270.
        //
        // > [Sapling to Canopy inclusive, pre-NU5] The transaction version number MUST be 4,
        // > and the version group ID MUST be 0x892F2085.
        //
        // > [NU5 onward] The transaction version number MUST be 4 or 5.
        // > If the transaction version number is 4 then the version group ID MUST be 0x892F2085.
        // > If the transaction version number is 5 then the version group ID MUST be 0x26A7270A.
        //
        // Note: Zebra checkpoints until Canopy blocks, this means only transactions versions
        // 4 and 5 get fully verified. This satisfies "The transaction version number MUST be 4"
        // and "The transaction version number MUST be 4 or 5" from the last two rules above.
        // This is done in the zebra-consensus crate, in the transactions checks.
        //
        // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
        match (version, overwintered) {
            (1, false) => Ok(Transaction::V1 {
                // Denoted as `tx_in_count` and `tx_in` in the spec.
                inputs: Vec::zcash_deserialize(&mut limited_reader)?,
                // Denoted as `tx_out_count` and `tx_out` in the spec.
                outputs: Vec::zcash_deserialize(&mut limited_reader)?,
                // Denoted as `lock_time` in the spec.
                lock_time: LockTime::zcash_deserialize(&mut limited_reader)?,
            }),
            (2, false) => {
                // Version 2 transactions use Sprout-on-BCTV14.
                type OptV2Jsd = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V2 {
                    // Denoted as `tx_in_count` and `tx_in` in the spec.
                    inputs: Vec::zcash_deserialize(&mut limited_reader)?,
                    // Denoted as `tx_out_count` and `tx_out` in the spec.
                    outputs: Vec::zcash_deserialize(&mut limited_reader)?,
                    // Denoted as `lock_time` in the spec.
                    lock_time: LockTime::zcash_deserialize(&mut limited_reader)?,
                    // A bundle of fields denoted in the spec as `nJoinSplit`, `vJoinSplit`,
                    // `joinSplitPubKey` and `joinSplitSig`.
                    joinsplit_data: OptV2Jsd::zcash_deserialize(&mut limited_reader)?,
                })
            }
            (3, true) => {
                // Denoted as `nVersionGroupId` in the spec.
                let id = limited_reader.read_u32::<LittleEndian>()?;
                if id != OVERWINTER_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse(
                        "expected OVERWINTER_VERSION_GROUP_ID",
                    ));
                }
                // Version 3 transactions use Sprout-on-BCTV14.
                type OptV3Jsd = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V3 {
                    // Denoted as `tx_in_count` and `tx_in` in the spec.
                    inputs: Vec::zcash_deserialize(&mut limited_reader)?,
                    // Denoted as `tx_out_count` and `tx_out` in the spec.
                    outputs: Vec::zcash_deserialize(&mut limited_reader)?,
                    // Denoted as `lock_time` in the spec.
                    lock_time: LockTime::zcash_deserialize(&mut limited_reader)?,
                    // Denoted as `nExpiryHeight` in the spec.
                    expiry_height: block::Height(limited_reader.read_u32::<LittleEndian>()?),
                    // A bundle of fields denoted in the spec as `nJoinSplit`, `vJoinSplit`,
                    // `joinSplitPubKey` and `joinSplitSig`.
                    joinsplit_data: OptV3Jsd::zcash_deserialize(&mut limited_reader)?,
                })
            }
            (4, true) => {
                // Transaction V4 spec:
                // https://zips.z.cash/protocol/protocol.pdf#txnencoding

                // Denoted as `nVersionGroupId` in the spec.
                let id = limited_reader.read_u32::<LittleEndian>()?;
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

                // Denoted as `tx_in_count` and `tx_in` in the spec.
                let inputs = Vec::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                let outputs = Vec::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `lock_time` in the spec.
                let lock_time = LockTime::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `nExpiryHeight` in the spec.
                let expiry_height = block::Height(limited_reader.read_u32::<LittleEndian>()?);

                // Denoted as `valueBalanceSapling` in the spec.
                let value_balance = (&mut limited_reader).zcash_deserialize_into()?;

                // Denoted as `nSpendsSapling` and `vSpendsSapling` in the spec.
                let shielded_spends = Vec::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `nOutputsSapling` and `vOutputsSapling` in the spec.
                let shielded_outputs =
                    Vec::<sapling::OutputInTransactionV4>::zcash_deserialize(&mut limited_reader)?
                        .into_iter()
                        .map(sapling::Output::from_v4)
                        .collect();

                // A bundle of fields denoted in the spec as `nJoinSplit`, `vJoinSplit`,
                // `joinSplitPubKey` and `joinSplitSig`.
                let joinsplit_data = OptV4Jsd::zcash_deserialize(&mut limited_reader)?;

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
                    // # Consensus
                    //
                    // > [Sapling onward] If effectiveVersion = 4 and there are no Spend
                    // > descriptions or Output descriptions, then valueBalanceSapling MUST be 0.
                    //
                    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
                    if value_balance != 0 {
                        return Err(SerializationError::BadTransactionBalance);
                    }
                    None
                };

                let sapling_shielded_data = match sapling_transfers {
                    Some(transfers) => Some(sapling::ShieldedData {
                        value_balance,
                        transfers,
                        // Denoted as `bindingSigSapling` in the spec.
                        binding_sig: limited_reader.read_64_bytes()?.into(),
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
                // Transaction V5 spec:
                // https://zips.z.cash/protocol/protocol.pdf#txnencoding

                // Denoted as `nVersionGroupId` in the spec.
                let id = limited_reader.read_u32::<LittleEndian>()?;
                if id != TX_V5_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse("expected TX_V5_VERSION_GROUP_ID"));
                }
                // Denoted as `nConsensusBranchId` in the spec.
                // Convert it to a NetworkUpgrade
                let network_upgrade =
                    NetworkUpgrade::from_branch_id(limited_reader.read_u32::<LittleEndian>()?)
                        .ok_or_else(|| {
                            SerializationError::Parse(
                                "expected a valid network upgrade from the consensus branch id",
                            )
                        })?;

                // Denoted as `lock_time` in the spec.
                let lock_time = LockTime::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `nExpiryHeight` in the spec.
                let expiry_height = block::Height(limited_reader.read_u32::<LittleEndian>()?);

                // Denoted as `tx_in_count` and `tx_in` in the spec.
                let inputs = Vec::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                let outputs = Vec::zcash_deserialize(&mut limited_reader)?;

                // A bundle of fields denoted in the spec as `nSpendsSapling`, `vSpendsSapling`,
                // `nOutputsSapling`,`vOutputsSapling`, `valueBalanceSapling`, `anchorSapling`,
                // `vSpendProofsSapling`, `vSpendAuthSigsSapling`, `vOutputProofsSapling` and
                // `bindingSigSapling`.
                let sapling_shielded_data = (&mut limited_reader).zcash_deserialize_into()?;

                // A bundle of fields denoted in the spec as `nActionsOrchard`, `vActionsOrchard`,
                // `flagsOrchard`,`valueBalanceOrchard`, `anchorOrchard`, `sizeProofsOrchard`,
                // `proofsOrchard`, `vSpendAuthSigsOrchard`, and `bindingSigOrchard`.
                let orchard_shielded_data = (&mut limited_reader).zcash_deserialize_into()?;

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
            #[cfg(feature = "tx-v6")]
            (6, true) => {
                // Transaction V6 spec:
                // https://zips.z.cash/zip-0230#transaction-format

                // Denoted as `nVersionGroupId` in the spec.
                let id = limited_reader.read_u32::<LittleEndian>()?;
                if id != TX_V6_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse("expected TX_V6_VERSION_GROUP_ID"));
                }
                // Denoted as `nConsensusBranchId` in the spec.
                // Convert it to a NetworkUpgrade
                let network_upgrade =
                    NetworkUpgrade::from_branch_id(limited_reader.read_u32::<LittleEndian>()?)
                        .ok_or_else(|| {
                            SerializationError::Parse(
                                "expected a valid network upgrade from the consensus branch id",
                            )
                        })?;

                // Denoted as `lock_time` in the spec.
                let lock_time = LockTime::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `nExpiryHeight` in the spec.
                let expiry_height = block::Height(limited_reader.read_u32::<LittleEndian>()?);

                // Denoted as `tx_in_count` and `tx_in` in the spec.
                let inputs = Vec::zcash_deserialize(&mut limited_reader)?;

                // Denoted as `tx_out_count` and `tx_out` in the spec.
                let outputs = Vec::zcash_deserialize(&mut limited_reader)?;

                // A bundle of fields denoted in the spec as `nSpendsSapling`, `vSpendsSapling`,
                // `nOutputsSapling`,`vOutputsSapling`, `valueBalanceSapling`, `anchorSapling`,
                // `vSpendProofsSapling`, `vSpendAuthSigsSapling`, `vOutputProofsSapling` and
                // `bindingSigSapling`.
                let sapling_shielded_data = (&mut limited_reader).zcash_deserialize_into()?;

                // A bundle of fields denoted in the spec as `nActionsOrchard`, `vActionsOrchard`,
                // `flagsOrchard`,`valueBalanceOrchard`, `anchorOrchard`, `sizeProofsOrchard`,
                // `proofsOrchard`, `vSpendAuthSigsOrchard`, and `bindingSigOrchard`.
                let orchard_shielded_data = (&mut limited_reader).zcash_deserialize_into()?;

                // OrchardZSA Issuance Fields.
                let orchard_zsa_issue_data = (&mut limited_reader).zcash_deserialize_into()?;

                Ok(Transaction::V6 {
                    network_upgrade,
                    lock_time,
                    expiry_height,
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    orchard_zsa_issue_data,
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

/// The minimum transaction size for v6 transactions.
///
#[allow(clippy::empty_line_after_doc_comments)]
/// FIXME: remove "clippy" line above, uncomment line below and specify a proper value and description.
//pub const MIN_TRANSPARENT_TX_V6_SIZE: u64 = MIN_TRANSPARENT_TX_V5_SIZE;

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
