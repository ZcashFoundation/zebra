//! Tachygrams - unified commitment/nullifier representation.
//!
//! Unlike Orchard which maintains separate trees for note commitments and nullifiers,
//! Tachyon uses a unified polynomial accumulator that tracks both via tachygrams.
//! This enables more efficient proofs in the recursive (PCD) context.

use ff::{Field, PrimeField};

use crate::note::{NoteCommitment, Nullifier};
use crate::primitives::Fp;

/// A tachygram is a generic data string representing either a note commitment
/// or a nullifier in the Tachyon polynomial accumulator.
///
/// The accumulator does not distinguish between commitments and nullifiers;
/// both are treated as field elements to be accumulated. This unified approach
/// simplifies the proof system and enables efficient batch operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tachygram(Fp);

impl Tachygram {
    /// Creates a new tachygram from a field element.
    pub fn from_field(value: Fp) -> Self {
        Self(value)
    }

    /// Returns the inner field element.
    pub fn inner(&self) -> Fp {
        self.0
    }

    /// Returns the byte representation of this tachygram.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_repr()
    }

    /// Creates a tachygram from bytes.
    ///
    /// Returns `None` if the bytes do not represent a valid field element.
    pub fn from_bytes(bytes: &[u8; 32]) -> Option<Self> {
        Fp::from_repr(*bytes).map(Self).into()
    }
}

impl From<Fp> for Tachygram {
    fn from(value: Fp) -> Self {
        Self(value)
    }
}

impl From<Tachygram> for Fp {
    fn from(tachygram: Tachygram) -> Self {
        tachygram.0
    }
}

impl From<NoteCommitment> for Tachygram {
    fn from(commitment: NoteCommitment) -> Self {
        Self(commitment.inner())
    }
}

impl From<Nullifier> for Tachygram {
    fn from(nullifier: Nullifier) -> Self {
        Self(nullifier.inner())
    }
}

impl Default for Tachygram {
    fn default() -> Self {
        Self(Fp::ZERO)
    }
}
