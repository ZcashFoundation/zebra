use std::io;

use reddsa::orchard::SpendAuth;

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
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
    pub rk: reddsa::VerificationKeyBytes<SpendAuth>,
    /// The x-coordinate of the note commitment for the output note.
    #[serde(with = "serde_helpers::ExtractedNoteCommitment")]
    pub cm_x: orchard::note::ExtractedNoteCommitment,
    /// An encoding of an ephemeral Pallas public key corresponding to the
    /// encrypted private key in `out_ciphertext`.
    pub ephemeral_key: keys::EphemeralPublicKey,
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: note::EncryptedNote,
    /// A ciphertext component that allows the holder of a full viewing key to
    /// recover the recipient diversified transmission key and the ephemeral
    /// private key (and therefore the entire note plaintext).
    pub out_ciphertext: note::WrappedNoteKey,
}

impl ZcashSerialize for Action {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
        writer.write_all(&<[u8; 32]>::from(self.nullifier)[..])?;
        writer.write_all(&<[u8; 32]>::from(self.rk)[..])?;
        writer.write_all(&<[u8; 32]>::from(&self.cm_x)[..])?;
        self.ephemeral_key.zcash_serialize(&mut writer)?;
        self.enc_ciphertext.zcash_serialize(&mut writer)?;
        self.out_ciphertext.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Action {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        // # Consensus
        //
        // > Elements of an Action description MUST be canonical encodings of the types given above.
        //
        // https://zips.z.cash/protocol/protocol.pdf#actiondesc
        //
        // > LEOS2IP_{256}(cmx) MUST be less than 𝑞_ℙ.
        //
        // https://zips.z.cash/protocol/protocol.pdf#actionencodingandconsensus
        //
        // See comments below for each specific type.
        Ok(Action {
            // Type is ValueCommit^{Orchard}.Output, i.e. ℙ.
            // https://zips.z.cash/protocol/protocol.pdf#abstractcommit
            // See [`ValueCommitment::zcash_deserialize`].
            cv: ValueCommitment::zcash_deserialize(&mut reader)?,
            // Type is `{0 .. 𝑞_ℙ − 1}`. See [`Nullifier::try_from`].
            nullifier: Nullifier::try_from(reader.read_32_bytes()?)?,
            // Type is SpendAuthSig^{Orchard}.Public, i.e. ℙ.
            // https://zips.z.cash/protocol/protocol.pdf#concretespendauthsig
            // https://zips.z.cash/protocol/protocol.pdf#concretereddsa
            // This only reads the 32-byte buffer. The type is enforced
            // on signature verification; see [`reddsa::batch`]
            rk: reader.read_32_bytes()?.into(),
            // Type is `{0 .. 𝑞_ℙ − 1}`. Note that the second rule quoted above
            // is also enforced here and it is technically redundant with the first.
            // See [`pallas::Base::zcash_deserialize`].
            cm_x: orchard::note::ExtractedNoteCommitment::zcash_deserialize(&mut reader)?,
            // Denoted by `epk` in the spec. Type is KA^{Orchard}.Public, i.e. ℙ^*.
            // https://zips.z.cash/protocol/protocol.pdf#concreteorchardkeyagreement
            // See [`keys::EphemeralPublicKey::zcash_deserialize`].
            ephemeral_key: keys::EphemeralPublicKey::zcash_deserialize(&mut reader)?,
            // Type is `Sym.C`, i.e. `𝔹^Y^{\[N\]}`, i.e. arbitrary-sized byte arrays
            // https://zips.z.cash/protocol/protocol.pdf#concretesym but fixed to
            // 580 bytes in https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
            // See [`note::EncryptedNote::zcash_deserialize`].
            enc_ciphertext: note::EncryptedNote::zcash_deserialize(&mut reader)?,
            // Type is `Sym.C`, i.e. `𝔹^Y^{\[N\]}`, i.e. arbitrary-sized byte arrays
            // https://zips.z.cash/protocol/protocol.pdf#concretesym but fixed to
            // 80 bytes in https://zips.z.cash/protocol/protocol.pdf#outputencodingandconsensus
            // See [`note::WrappedNoteKey::zcash_deserialize`].
            out_ciphertext: note::WrappedNoteKey::zcash_deserialize(&mut reader)?,
        })
    }
}

/// TODO
impl ZcashDeserialize for orchard::note::ExtractedNoteCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        orchard::note::ExtractedNoteCommitment::from_bytes(&reader.read_32_bytes()?)
            .into_option()
            .ok_or(SerializationError::Parse(
                "Invalid pallas::Base, input not canonical",
            ))
    }
}
