//! Tachyon notes and nullifiers.
//!
//! Notes represent discrete amounts of value within the Tachyon shielded pool.
//! When spent, notes produce nullifiers that are tracked via tachygrams in the
//! polynomial accumulator.

use ff::PrimeField;

use crate::primitives::Fp;

/// A Tachyon note.
///
/// Notes represent discrete amounts of value within the Tachyon shielded pool.
/// Each note contains:
/// - A value amount
/// - A recipient address
/// - Randomness for hiding the note commitment
///
/// This is a stub type for progressive implementation.
#[derive(Clone, Debug)]
pub struct Note {
    /// The value of this note in zatoshis.
    value: u64,
    /// Random seed for note randomness derivation.
    rseed: Fp,
}

impl Note {
    /// Creates a new note with the given value and randomness.
    pub fn new(value: u64, rseed: Fp) -> Self {
        Self { value, rseed }
    }

    /// Returns the value of this note.
    pub fn value(&self) -> u64 {
        self.value
    }

    /// Returns the random seed of this note.
    pub fn rseed(&self) -> Fp {
        self.rseed
    }
}

/// The nullifier trapdoor $\Psi$ for a Tachyon note.
///
/// This is user-controlled randomness that, combined with the nullifier key
/// and epoch flavor, produces a unique nullifier for the note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NullifierTrapdoor(Fp);

impl NullifierTrapdoor {
    /// Creates a new nullifier trapdoor from a field element.
    pub fn from_field(value: Fp) -> Self {
        Self(value)
    }

    /// Returns the inner field element.
    pub fn inner(&self) -> Fp {
        self.0
    }
}

/// An epoch identifier (flavor) $e$ for nullifier derivation.
///
/// The epoch is used in the GGM Tree PRF construction to enable
/// constrained nullifier key delegation. A constrained key $\mathsf{nk}_t$ can only
/// compute nullifiers for epochs $e \leq t$.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(u64);

impl Epoch {
    /// Creates a new epoch from a 64-bit value.
    pub const fn new(epoch: u64) -> Self {
        Self(epoch)
    }

    /// Returns the epoch as a u64.
    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    /// Converts the epoch to a field element for use in PRF computation.
    pub fn to_field(&self) -> Fp {
        Fp::from(self.0)
    }
}

/// A nullifier for a spent Tachyon note.
///
/// Tachyon uses a simplified nullifier design compared to Orchard:
///
/// $$\mathsf{nf} = F_{\mathsf{nk}}(\Psi \| e)$$
///
/// where:
/// - $\mathsf{nk}$ is the nullifier key
/// - $\Psi$ is the nullifier trapdoor (user-controlled randomness)
/// - $e$ is the epoch (flavor)
///
/// This design enables constrained PRFs for oblivious syncing delegation:
/// a delegated key $\mathsf{nk}_t$ can only compute nullifiers for epochs $e \leq t$.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nullifier(Fp);

impl Nullifier {
    /// Creates a nullifier from a field element.
    pub fn from_field(value: Fp) -> Self {
        Self(value)
    }

    /// Returns the inner field element.
    pub fn inner(&self) -> Fp {
        self.0
    }

    /// Returns the byte representation of this nullifier.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_repr()
    }

    /// Creates a nullifier from bytes.
    ///
    /// Returns `None` if the bytes do not represent a valid field element.
    pub fn from_bytes(bytes: &[u8; 32]) -> Option<Self> {
        Fp::from_repr(*bytes).map(Self).into()
    }
}

impl From<Fp> for Nullifier {
    fn from(value: Fp) -> Self {
        Self(value)
    }
}

impl From<Nullifier> for Fp {
    fn from(nullifier: Nullifier) -> Self {
        nullifier.0
    }
}

/// A note commitment.
///
/// This is a hiding commitment to a note, stored in the polynomial accumulator
/// as a tachygram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteCommitment(Fp);

impl NoteCommitment {
    /// Creates a note commitment from a field element.
    pub fn from_field(value: Fp) -> Self {
        Self(value)
    }

    /// Returns the inner field element.
    pub fn inner(&self) -> Fp {
        self.0
    }

    /// Returns the byte representation.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_repr()
    }
}

impl From<Fp> for NoteCommitment {
    fn from(value: Fp) -> Self {
        Self(value)
    }
}
