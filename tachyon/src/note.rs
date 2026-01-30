//! Tachyon notes.

/// A Tachyon note.
///
/// Notes represent discrete amounts of value within the Tachyon shielded pool.
/// When spent, notes produce nullifiers that are tracked via tachygrams in the
/// polynomial accumulator.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug)]
pub struct Note {
    // Future fields: value, recipient, rho, rseed, etc.
    _private: (),
}

/// A nullifier for a spent Tachyon note.
///
/// Tachyon uses a simplified nullifier design compared to Orchard:
/// `nf = Fnk(Ψ || flavor)` where:
/// - Ψ is the nullifier trapdoor (user-controlled randomness)
/// - `flavor` is the epoch-ID
///
/// This enables constrained PRFs for oblivious syncing delegation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nullifier([u8; 32]);

impl Nullifier {
    /// Returns the byte representation of this nullifier.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }
}

impl From<[u8; 32]> for Nullifier {
    fn from(bytes: [u8; 32]) -> Self {
        Nullifier(bytes)
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(nullifier: Nullifier) -> Self {
        nullifier.0
    }
}

impl AsRef<[u8; 32]> for Nullifier {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}
