//! Sapling _Output descriptions_, as described in [protocol specification §7.4][ps].
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding

use std::io;

use crate::{
    block::MAX_BLOCK_BYTES,
    primitives::Groth16Proof,
    serialization::{
        serde_helpers, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    },
};

use super::{commitment, keys, note};

/// A _Output Description_, as described in [protocol specification §7.4][ps].
///
/// # Differences between Transaction Versions
///
/// `V4` transactions serialize the fields of spends and outputs together.
/// `V5` transactions split them into multiple arrays.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// A value commitment to the value of the input note.
    pub cv: commitment::ValueCommitment,
    /// The u-coordinate of the note commitment for the output note.
    #[serde(with = "serde_helpers::SaplingExtractedNoteCommitment")]
    pub cm_u: sapling_crypto::note::ExtractedNoteCommitment,
    /// An encoding of an ephemeral Jubjub public key.
    pub ephemeral_key: keys::EphemeralPublicKey,
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: note::EncryptedNote,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: note::WrappedNoteKey,
    /// The ZK output proof.
    pub zkproof: Groth16Proof,
}

/// Wrapper for `Output` serialization in a `V4` transaction.
///
/// <https://zips.z.cash/protocol/protocol.pdf#outputencoding>
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputInTransactionV4(pub Output);

/// The serialization prefix fields of an `Output` in Transaction V5.
///
/// In `V5` transactions, spends are split into multiple arrays, so the prefix
/// and proof must be serialised and deserialized separately.
///
/// Serialized as `OutputDescriptionV5` in [protocol specification §7.3][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputPrefixInTransactionV5 {
    /// A value commitment to the value of the input note.
    pub cv: commitment::ValueCommitment,
    /// The u-coordinate of the note commitment for the output note.
    #[serde(with = "serde_helpers::SaplingExtractedNoteCommitment")]
    pub cm_u: sapling_crypto::note::ExtractedNoteCommitment,
    /// An encoding of an ephemeral Jubjub public key.
    pub ephemeral_key: keys::EphemeralPublicKey,
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: note::EncryptedNote,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: note::WrappedNoteKey,
}

impl Output {
    /// Remove the V4 transaction wrapper from this output.
    pub fn from_v4(output: OutputInTransactionV4) -> Output {
        output.0
    }

    /// Add a V4 transaction wrapper to this output.
    pub fn into_v4(self) -> OutputInTransactionV4 {
        OutputInTransactionV4(self)
    }

    /// Combine the prefix and non-prefix fields from V5 transaction
    /// deserialization.
    pub fn from_v5_parts(prefix: OutputPrefixInTransactionV5, zkproof: Groth16Proof) -> Output {
        Output {
            cv: prefix.cv,
            cm_u: prefix.cm_u,
            ephemeral_key: prefix.ephemeral_key,
            enc_ciphertext: prefix.enc_ciphertext,
            out_ciphertext: prefix.out_ciphertext,
            zkproof,
        }
    }

    /// Split out the prefix and non-prefix fields for V5 transaction
    /// serialization.
    pub fn into_v5_parts(self) -> (OutputPrefixInTransactionV5, Groth16Proof) {
        let prefix = OutputPrefixInTransactionV5 {
            cv: self.cv,
            cm_u: self.cm_u,
            ephemeral_key: self.ephemeral_key,
            enc_ciphertext: self.enc_ciphertext,
            out_ciphertext: self.out_ciphertext,
        };

        (prefix, self.zkproof)
    }
}

impl OutputInTransactionV4 {
    /// Add V4 transaction wrapper to this output.
    pub fn from_output(output: Output) -> OutputInTransactionV4 {
        OutputInTransactionV4(output)
    }

    /// Remove the V4 transaction wrapper from this output.
    pub fn into_output(self) -> Output {
        self.0
    }
}

impl ZcashSerialize for OutputInTransactionV4 {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let output = self.0.clone();
        writer.write_all(&output.cv.0.to_bytes())?;
        writer.write_all(&output.cm_u.to_bytes())?;
        output.ephemeral_key.zcash_serialize(&mut writer)?;
        output.enc_ciphertext.zcash_serialize(&mut writer)?;
        output.out_ciphertext.zcash_serialize(&mut writer)?;
        output.zkproof.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for OutputInTransactionV4 {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // # Consensus
        //
        // > Elements of an Output description MUST be valid encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#outputdesc
        //
        // > LEOS2IP_{256}(cmu) MUST be less than 𝑞_J.
        //
        // https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
        //
        // See comments below for each specific type.
        Ok(OutputInTransactionV4(Output {
            // Type is `ValueCommit^{Sapling}.Output`, i.e. J
            // https://zips.z.cash/protocol/protocol.pdf#abstractcommit
            // See [`commitment::NotSmallOrderValueCommitment::zcash_deserialize`].
            cv: commitment::ValueCommitment(
                sapling_crypto::value::ValueCommitment::zcash_deserialize(&mut reader)?,
            ),
            // Type is `B^{[ℓ_{Sapling}_{Merkle}]}`, i.e. 32 bytes.
            // However, the consensus rule above restricts it even more.
            // See [`jubjub::Fq::zcash_deserialize`].
            cm_u: sapling_crypto::note::ExtractedNoteCommitment::zcash_deserialize(&mut reader)?,
            // Type is `KA^{Sapling}.Public`, i.e. J
            // https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement
            // See [`keys::EphemeralPublicKey::zcash_deserialize`].
            ephemeral_key: keys::EphemeralPublicKey::zcash_deserialize(&mut reader)?,
            // Type is `Sym.C`, i.e. `B^Y^{\[N\]}`, i.e. arbitrary-sized byte arrays
            // https://zips.z.cash/protocol/protocol.pdf#concretesym but fixed to
            // 580 bytes in https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
            // See [`note::EncryptedNote::zcash_deserialize`].
            enc_ciphertext: note::EncryptedNote::zcash_deserialize(&mut reader)?,
            // Type is `Sym.C`, i.e. `B^Y^{\[N\]}`, i.e. arbitrary-sized byte arrays.
            // https://zips.z.cash/protocol/protocol.pdf#concretesym but fixed to
            // 80 bytes in https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
            // See [`note::WrappedNoteKey::zcash_deserialize`].
            out_ciphertext: note::WrappedNoteKey::zcash_deserialize(&mut reader)?,
            // Type is `ZKOutput.Proof`, described in
            // https://zips.z.cash/protocol/protocol.pdf#grothencoding
            // It is not enforced here; this just reads 192 bytes.
            // The type is validated when validating the proof, see
            // [`groth16::Item::try_from`]. In #3179 we plan to validate here instead.
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
        }))
    }
}

// In a V5 transaction, zkproof is deserialized separately, so we can only
// deserialize V5 outputs in the context of a V5 transaction.
//
// Instead, implement serialization and deserialization for the
// Output prefix fields, which are stored in the same array.

impl ZcashSerialize for OutputPrefixInTransactionV5 {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.cv.0.to_bytes())?;
        writer.write_all(&self.cm_u.to_bytes())?;
        self.ephemeral_key.zcash_serialize(&mut writer)?;
        self.enc_ciphertext.zcash_serialize(&mut writer)?;
        self.out_ciphertext.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for OutputPrefixInTransactionV5 {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // # Consensus
        //
        // > Elements of an Output description MUST be valid encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#outputdesc
        //
        // > LEOS2IP_{256}(cmu) MUST be less than 𝑞_J.
        //
        // https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
        //
        // See comments below for each specific type.
        Ok(OutputPrefixInTransactionV5 {
            // Type is `ValueCommit^{Sapling}.Output`, i.e. J
            // https://zips.z.cash/protocol/protocol.pdf#abstractcommit
            // See [`commitment::NotSmallOrderValueCommitment::zcash_deserialize`].
            cv: commitment::ValueCommitment(
                sapling_crypto::value::ValueCommitment::zcash_deserialize(&mut reader)?,
            ),
            // Type is `B^{[ℓ_{Sapling}_{Merkle}]}`, i.e. 32 bytes.
            // However, the consensus rule above restricts it even more.
            // See [`jubjub::Fq::zcash_deserialize`].
            cm_u: sapling_crypto::note::ExtractedNoteCommitment::zcash_deserialize(&mut reader)?,
            // Type is `KA^{Sapling}.Public`, i.e. J
            // https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement
            // See [`keys::EphemeralPublicKey::zcash_deserialize`].
            ephemeral_key: keys::EphemeralPublicKey::zcash_deserialize(&mut reader)?,
            // Type is `Sym.C`, i.e. `B^Y^{\[N\]}`, i.e. arbitrary-sized byte arrays
            // https://zips.z.cash/protocol/protocol.pdf#concretesym but fixed to
            // 580 bytes in https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
            // See [`note::EncryptedNote::zcash_deserialize`].
            enc_ciphertext: note::EncryptedNote::zcash_deserialize(&mut reader)?,
            // Type is `Sym.C`, i.e. `B^Y^{\[N\]}`, i.e. arbitrary-sized byte arrays.
            // https://zips.z.cash/protocol/protocol.pdf#concretesym but fixed to
            // 80 bytes in https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
            // See [`note::WrappedNoteKey::zcash_deserialize`].
            out_ciphertext: note::WrappedNoteKey::zcash_deserialize(&mut reader)?,
        })
    }
}

/// The size of a v5 output, without associated fields.
///
/// This is the size of outputs in the initial array, there is another
/// array of zkproofs required in the transaction format.
pub(crate) const OUTPUT_PREFIX_SIZE: u64 = 32 + 32 + 32 + 580 + 80;
/// An output contains: a 32 byte cv, a 32 byte cmu, a 32 byte ephemeral key
/// a 580 byte encCiphertext, an 80 byte outCiphertext, and a 192 byte zkproof
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
pub(crate) const OUTPUT_SIZE: u64 = OUTPUT_PREFIX_SIZE + 192;

/// The maximum number of sapling outputs in a valid Zcash on-chain transaction.
/// This maximum is the same for transaction V4 and V5, even though the fields are
/// serialized in a different order.
///
/// If a transaction contains more outputs than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus
impl TrustedPreallocate for OutputInTransactionV4 {
    fn max_allocation() -> u64 {
        // Since a serialized Vec<Output> uses at least one byte for its length,
        // the max allocation can never exceed (MAX_BLOCK_BYTES - 1) / OUTPUT_SIZE
        const MAX: u64 = (MAX_BLOCK_BYTES - 1) / OUTPUT_SIZE;
        // # Consensus
        //
        // > [NU5 onward] nSpendsSapling, nOutputsSapling, and nActionsOrchard MUST all be less than 2^16.
        //
        // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
        //
        // This acts as nOutputsSapling and is therefore subject to the rule.
        // The maximum value is actually smaller due to the block size limit,
        // but we ensure the 2^16 limit with a static assertion.
        // (The check is not required pre-NU5, but it doesn't cause problems.)
        static_assertions::const_assert!(MAX < (1 << 16));
        MAX
    }
}

impl TrustedPreallocate for OutputPrefixInTransactionV5 {
    fn max_allocation() -> u64 {
        // Since V4 and V5 have the same fields,
        // and the V5 associated fields are required,
        // a valid max allocation can never exceed this size
        OutputInTransactionV4::max_allocation()
    }
}
