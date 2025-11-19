use std::io;

use crate::{
    sapling,
    serialization::{
        zcash_deserialize_external_count, zcash_serialize_empty_list,
        zcash_serialize_external_count, ReadZcashExt, SerializationError, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
    versioned_sig::VersionedSig,
};

use redjubjub::{Binding, Signature, SpendAuth};

type SaplingShieldedData = sapling::ShieldedData<sapling::SharedAnchor>;

impl ZcashSerialize for Signature<Binding> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 64]>::from(*self)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for Signature<Binding> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_64_bytes()?.into())
    }
}

// Transaction::V5 serializes sapling ShieldedData in a single continuous byte
// range, so we can implement its serialization and deserialization separately.
// (Unlike V4, where it must be serialized as part of the transaction.)

pub(super) fn zcash_serialize_v6<W: io::Write>(
    shielded_data: &Option<SaplingShieldedData>,
    mut writer: W,
) -> Result<(), io::Error> {
    match shielded_data {
        None => {
            // Denoted as `nSpendsSapling` in the spec.
            zcash_serialize_empty_list(&mut writer)?;
            // Denoted as `nOutputsSapling` in the spec.
            zcash_serialize_empty_list(&mut writer)?;
        }
        Some(sapling_shielded_data) => {
            zcash_serialize_v6_inner(sapling_shielded_data, &mut writer)?;
        }
    }
    Ok(())
}

fn zcash_serialize_v6_inner<W: io::Write>(
    shielded_data: &SaplingShieldedData,
    mut writer: W,
) -> Result<(), io::Error> {
    // Collect arrays for Spends
    // There's no unzip3, so we have to unzip twice.
    let (spend_prefixes, spend_proofs_sigs): (Vec<_>, Vec<_>) = shielded_data
        .spends()
        .cloned()
        .map(sapling::Spend::<sapling::SharedAnchor>::into_v5_parts)
        .map(|(prefix, proof, sig)| (prefix, (proof, VersionedSig::for_tx_v6(sig))))
        .unzip();
    let (spend_proofs, spend_sigs) = spend_proofs_sigs.into_iter().unzip();

    // Collect arrays for Outputs
    let (output_prefixes, output_proofs): (Vec<_>, _) = shielded_data
        .outputs()
        .cloned()
        .map(sapling::Output::into_v5_parts)
        .unzip();

    // Denoted as `nSpendsSapling` and `vSpendsSapling` in the spec.
    spend_prefixes.zcash_serialize(&mut writer)?;
    // Denoted as `nOutputsSapling` and `vOutputsSapling` in the spec.
    output_prefixes.zcash_serialize(&mut writer)?;

    // Denoted as `valueBalanceSapling` in the spec.
    shielded_data.value_balance.zcash_serialize(&mut writer)?;

    // Denoted as `anchorSapling` in the spec.
    // `TransferData` ensures this field is only present when there is at
    // least one spend.
    if let Some(shared_anchor) = shielded_data.shared_anchor() {
        writer.write_all(&<[u8; 32]>::from(shared_anchor)[..])?;
    }

    // Denoted as `vSpendProofsSapling` in the spec.
    zcash_serialize_external_count(&spend_proofs, &mut writer)?;
    // Denoted as `vSpendAuthSigsSapling` in the spec.
    zcash_serialize_external_count(&spend_sigs, &mut writer)?;

    // Denoted as `vOutputProofsSapling` in the spec.
    zcash_serialize_external_count(&output_proofs, &mut writer)?;

    // Denoted as `bindingSigSapling` in the spec.
    VersionedSig::for_tx_v6(shielded_data.binding_sig).zcash_serialize(&mut writer)?;

    Ok(())
}

// we can't split ShieldedData out of Option<ShieldedData> deserialization,
// because the counts are read along with the arrays.
#[allow(clippy::unwrap_in_result)]
pub(super) fn zcash_deserialize_v6<R: io::Read>(
    mut reader: R,
) -> Result<Option<SaplingShieldedData>, SerializationError> {
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
    let spend_sigs: Vec<VersionedSig<Signature<SpendAuth>>> =
        zcash_deserialize_external_count(spends_count, &mut reader)?;

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
    let binding_sig = VersionedSig::zcash_deserialize(&mut reader)?.as_tx_v6()?;

    // Create shielded spends from deserialized parts
    let spends: Vec<_> = spend_prefixes
        .into_iter()
        .zip(spend_proofs)
        .zip(spend_sigs)
        .map(|((prefix, proof), spend_sig)| {
            spend_sig.as_tx_v6().map(|sig| {
                sapling::Spend::<sapling::SharedAnchor>::from_v5_parts(prefix, proof, sig)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

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
