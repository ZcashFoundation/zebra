//! Tachygrams - unified commitment/nullifier representation.
//!
//! Unlike Orchard which maintains separate trees for note commitments and nullifiers,
//! Tachyon uses a unified polynomial accumulator that tracks both via tachygrams.
//! This enables more efficient proofs in the recursive (PCD) context.

/// A tachygram is a generic data string representing either a note commitment
/// or a nullifier in the Tachyon polynomial accumulator.
///
/// The accumulator does not distinguish between commitments and nullifiers;
/// both are treated as elements to be accumulated. This unified approach
/// simplifies the proof system and enables efficient batch operations.
///
/// This is a stub type for future implementation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tachygram([u8; 32]);

impl Tachygram {
    /// Creates a new tachygram from bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Tachygram(bytes)
    }

    /// Returns the byte representation of this tachygram.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl From<[u8; 32]> for Tachygram {
    fn from(bytes: [u8; 32]) -> Self {
        Tachygram(bytes)
    }
}

impl From<Tachygram> for [u8; 32] {
    fn from(tachygram: Tachygram) -> Self {
        tachygram.0
    }
}

impl AsRef<[u8; 32]> for Tachygram {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}
