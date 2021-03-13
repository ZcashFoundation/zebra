use std::io;

use halo2::{arithmetic::FieldExt, pasta::pallas};

use crate::{
    primitives::redpallas::{self, SpendAuth},
    serialization::{
        ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
    },
};

use super::{
    commitment::{self, ValueCommitment},
    keys,
    note::{self, Nullifier},
};

/// An Action description, as described in the [Zcash specification §7.3][actiondesc].
///
/// Action transfers can optionally perform a spend, and optionally perform an
/// output.  Action descriptions are data included in a transaction that
/// describe Action transfers.
///
/// [actiondesc]: https://zips.z.cash/protocol/nu5.pdf#actiondesc
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    /// A value commitment to net value of the input note minus the output note
    pub cv: commitment::ValueCommitment,
    /// The nullifier of the input note being spent.
    pub nullifier: note::Nullifier,
    /// The randomized validating key for spendAuthSig,
    pub rk: redpallas::VerificationKeyBytes<SpendAuth>,
    /// The 𝑥-coordinate of the note commitment for the output note.
    pub cm_x: pallas::Base,
    /// An encoding of an ephemeral Pallas public key.
    pub ephemeral_key: keys::EphemeralPublicKey,
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: note::EncryptedNote,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: note::WrappedNoteKey,
}

impl ZcashSerialize for Action {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
        writer.write_32_bytes(self.nullifier.into())?;
        writer.write_all(&<[u8; 32]>::from(self.rk)[..])?;
        writer.write_all(self.cm_x.to_bytes())?;
        self.ephemeral_key.zcash_serialize(&mut writer)?;
        self.enc_ciphertext.zcash_serialize(&mut writer)?;
        self.out_ciphertext.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Action {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Action {
            cv: ValueCommitment::zcash_deserialize(&mut reader)?,
            nullifier: Nullifier::from(reader.read_32_bytes()?),
            rk: reader.read_32_bytes()?.into(),
            cm_x: pallas::Base::zcash_deserialize(&mut reader)?,
            ephemeral_key: keys::EphemeralPublicKey::zcash_deserialize(&mut reader)?,
            enc_ciphertext: note::EncryptedNote::zcash_deserialize(&mut reader)?,
            out_ciphertext: note::WrappedNoteKey::zcash_deserialize(&mut reader)?,
        })
    }
}
