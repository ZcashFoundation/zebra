use std::io;

use crate::{
    primitives::Groth16Proof,
    serialization::{serde_helpers, SerializationError, ZcashDeserialize, ZcashSerialize},
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
