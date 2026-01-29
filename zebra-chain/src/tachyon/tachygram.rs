//! Tachygram: A unified 32-byte representation for nullifiers and note commitments.
//!
//! Tachygrams form the basis of the Tachyon commitment tree, enabling a single
//! accumulator structure for both nullifiers and note commitments.

use std::{fmt, io};

use crate::serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize};

use super::{commitment::NoteCommitment, nullifier::Nullifier};

/// A unified 32-byte representation that can be either a nullifier or note commitment.
///
/// Tachygrams form the basis of the Tachyon commitment tree. Both nullifiers and
/// note commitments are represented as 32-byte blobs, enabling a single polynomial
/// accumulator for set (non-)membership proofs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tachygram(pub(crate) [u8; 32]);

impl Tachygram {
    /// The size of a serialized Tachygram in bytes.
    pub const SIZE: usize = 32;

    /// Create a new Tachygram from raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Get the raw bytes of this Tachygram.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// Create a Tachygram from a Tachyon nullifier.
    ///
    /// Note: The epoch is not included in the tachygram representation.
    /// The epoch is used for wallet scanning but the core nullifier value
    /// is what gets committed to the accumulator.
    pub fn from_nullifier(nf: &Nullifier) -> Self {
        Self(nf.to_bytes())
    }

    /// Create a Tachygram from a Tachyon note commitment.
    pub fn from_note_commitment(cm: &NoteCommitment) -> Self {
        Self(cm.extract_x_bytes())
    }
}

impl fmt::Debug for Tachygram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Tachygram")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl From<[u8; 32]> for Tachygram {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<Tachygram> for [u8; 32] {
    fn from(tg: Tachygram) -> Self {
        tg.0
    }
}

impl AsRef<[u8; 32]> for Tachygram {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl ZcashSerialize for Tachygram {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0)
    }
}

impl ZcashDeserialize for Tachygram {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(reader.read_32_bytes()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tachygram_roundtrip() {
        let _init_guard = zebra_test::init();

        let bytes = [42u8; 32];
        let tg = Tachygram::from_bytes(bytes);
        assert_eq!(tg.to_bytes(), bytes);
    }
}
