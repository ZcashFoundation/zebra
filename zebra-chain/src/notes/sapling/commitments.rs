use std::{fmt, io};

use crate::{
    serde_helpers,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// The randomness used in the Pedersen Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CommitmentRandomness(redjubjub::Randomizer);

/// Note commitments for the output notes.
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
//#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct NoteCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for NoteCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NoteCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<[u8; 32]> for NoteCommitment {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl Eq for NoteCommitment {}

impl From<NoteCommitment> for [u8; 32] {
    fn from(cm: NoteCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

impl ZcashSerialize for NoteCommitment {
    // The u-coordinate of the note commitment, for the output note
    // LEBS2OSP256(cm_u) where cm_u = Extract_J(r)(cm). ???
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())?;
        Ok(())
    }
}

impl ZcashDeserialize for NoteCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(
            jubjub::AffinePoint::from_bytes(reader.read_32_bytes()?).unwrap(),
        ))
    }
}

impl NoteCommitment {
    /// Hash Extractor for Jubjub (?)
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concreteextractorjubjub
    pub fn extract_u(self) -> jubjub::Fq {
        self.0.get_u()
    }
}

/// A Homomorphic Pedersen commitment to the value of a note, used in
/// Spend and Output Descriptions.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
#[derive(Clone, Deserialize, PartialEq, Serialize)]
//#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct ValueCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ValueCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<[u8; 32]> for ValueCommitment {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl Eq for ValueCommitment {}

impl From<ValueCommitment> for [u8; 32] {
    fn from(cm: ValueCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

/// LEBS2OSP256(repr_J(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#spendencoding
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())?;
        Ok(())
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(
            jubjub::AffinePoint::from_bytes(reader.read_32_bytes()?).unwrap(),
        ))
    }
}
