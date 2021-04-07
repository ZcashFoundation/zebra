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
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// A value commitment to the value of the input note.
    pub cv: commitment::ValueCommitment,
    /// The u-coordinate of the note commitment for the output note.
    #[serde(with = "serde_helpers::Fq")]
    pub cm_u: jubjub::Fq,
    /// An encoding of an ephemeral Jubjub public key.
    pub ephemeral_key: keys::EphemeralPublicKey,
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: note::EncryptedNote,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: note::WrappedNoteKey,
    /// The ZK output proof.
    pub zkproof: Groth16Proof,
}

impl Output {
    /// Encodes the primary inputs for the proof statement as 5 Bls12_381 base
    /// field elements, to match bellman::groth16::verify_proof.
    ///
    /// NB: jubjub::Fq is a type alias for bls12_381::Scalar.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#cctsaplingoutput
    pub fn primary_inputs(&self) -> Vec<jubjub::Fq> {
        let mut inputs = vec![];

        let cv_affine = jubjub::AffinePoint::from_bytes(self.cv.into()).unwrap();
        inputs.push(cv_affine.get_u());
        inputs.push(cv_affine.get_v());

        let epk_affine = jubjub::AffinePoint::from_bytes(self.ephemeral_key.into()).unwrap();
        inputs.push(epk_affine.get_u());
        inputs.push(epk_affine.get_v());

        inputs.push(self.cm_u);

        inputs
    }
}

impl ZcashSerialize for Output {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
        writer.write_all(&self.cm_u.to_bytes())?;
        self.ephemeral_key.zcash_serialize(&mut writer)?;
        self.enc_ciphertext.zcash_serialize(&mut writer)?;
        self.out_ciphertext.zcash_serialize(&mut writer)?;
        self.zkproof.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Output {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Output {
            cv: commitment::ValueCommitment::zcash_deserialize(&mut reader)?,
            cm_u: jubjub::Fq::zcash_deserialize(&mut reader)?,
            ephemeral_key: keys::EphemeralPublicKey::zcash_deserialize(&mut reader)?,
            enc_ciphertext: note::EncryptedNote::zcash_deserialize(&mut reader)?,
            out_ciphertext: note::WrappedNoteKey::zcash_deserialize(&mut reader)?,
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
        })
    }
}

/// An output contains: a 32 byte cv, a 32 byte cmu, a 32 byte ephemeral key
/// a 580 byte encCiphertext, an 80 byte outCiphertext, and a 192 byte zkproof
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
pub(crate) const OUTPUT_SIZE: u64 = 32 + 32 + 32 + 580 + 80 + 192;

/// The maximum number of outputs in a valid Zcash on-chain transaction.
///
/// If a transaction contains more outputs than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus
impl TrustedPreallocate for Output {
    fn max_allocation() -> u64 {
        // Since a serialized Vec<Output> uses at least one byte for its length,
        // the max allocation can never exceed (MAX_BLOCK_BYTES - 1) / OUTPUT_SIZE
        (MAX_BLOCK_BYTES - 1) / OUTPUT_SIZE
    }
}
