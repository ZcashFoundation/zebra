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
    pub enc_ciphertext: note::EncryptedCiphertext,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: note::OutCiphertext,
    /// The ZK output proof.
    pub zkproof: Groth16Proof,
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
            enc_ciphertext: note::EncryptedCiphertext::zcash_deserialize(&mut reader)?,
            out_ciphertext: note::OutCiphertext::zcash_deserialize(&mut reader)?,
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
        })
    }
}
