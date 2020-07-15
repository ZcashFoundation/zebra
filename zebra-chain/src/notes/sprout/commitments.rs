use std::io;

use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};

/// The randomness used in the Pedersen Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct CommitmentRandomness(pub [u8; 32]);

impl AsRef<[u8]> for CommitmentRandomness {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Note commitments for the output notes.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct NoteCommitment(pub(crate) [u8; 32]);

impl ZcashSerialize for NoteCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for NoteCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(reader.read_32_bytes()?))
    }
}
