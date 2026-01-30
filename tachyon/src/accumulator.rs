//! Tachyon polynomial accumulator.
//!
//! Unlike Orchard's Merkle tree-based note commitment tree, Tachyon uses a
//! polynomial-commitment accumulator. The accumulator is a commitment to a
//! polynomial with roots at the committed values (tachygrams), hashed with
//! the previous accumulator value.
//!
//! This design supports efficient set membership proofs in recursive (PCD)
//! contexts and unifies tracking of both note commitments and nullifiers.

use crate::Tachygram;

/// The root of the Tachyon polynomial accumulator.
///
/// This serves as the anchor for Tachyon transactions, analogous to
/// Orchard's `Anchor` but representing a polynomial commitment rather
/// than a Merkle root.
///
/// The accumulator root and tachygrams serve as public inputs to the
/// Ragu proof verification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccumulatorRoot([u8; 32]);

impl AccumulatorRoot {
    /// Creates a new accumulator root from bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        AccumulatorRoot(bytes)
    }

    /// Returns the byte representation of this accumulator root.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl From<[u8; 32]> for AccumulatorRoot {
    fn from(bytes: [u8; 32]) -> Self {
        AccumulatorRoot(bytes)
    }
}

impl From<AccumulatorRoot> for [u8; 32] {
    fn from(root: AccumulatorRoot) -> Self {
        root.0
    }
}

impl AsRef<[u8; 32]> for AccumulatorRoot {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A Tachyon polynomial accumulator.
///
/// The accumulator maintains a commitment to all tachygrams (note commitments
/// and nullifiers) in the Tachyon shielded pool. It supports:
/// - Efficient batch updates
/// - Set membership proofs for the Ragu proof system
/// - State pruning by validators
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug, Default)]
pub struct Accumulator {
    /// The current root of the accumulator.
    root: AccumulatorRoot,
    // Future fields: polynomial commitment state, etc.
}

impl Accumulator {
    /// Creates a new empty accumulator.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns the current root of the accumulator.
    pub fn root(&self) -> AccumulatorRoot {
        self.root
    }

    /// Appends a tachygram to the accumulator.
    ///
    /// This is a stub that does not actually update the accumulator state.
    pub fn append(&mut self, _tachygram: Tachygram) {
        // Future implementation will update the polynomial commitment
    }
}
