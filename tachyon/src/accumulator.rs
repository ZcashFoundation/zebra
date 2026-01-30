//! Tachyon polynomial accumulator.
//!
//! Unlike Orchard's Merkle tree-based note commitment tree, Tachyon uses a
//! polynomial-commitment accumulator that unifies note commitments and nullifiers
//! into a single data structure.
//!
//! ## Polynomial Representation
//!
//! The accumulator commits to a polynomial $p(X)$ with roots at the accumulated
//! tachygrams:
//!
//! $$p(X) = \prod_{i=0}^{n-1} (X - t_i)$$
//!
//! where $t_i \in \mathbb{F}_p$ are the tachygrams (note commitments or nullifiers).
//!
//! ## Advantages
//!
//! - **Unified tracking**: Both commitments and nullifiers use the same structure
//! - **Efficient membership proofs**: Polynomial evaluation proofs are compact
//! - **PCD-friendly**: Works naturally with recursive proof composition

use ff::{Field, PrimeField};

use crate::Tachygram;
use crate::primitives::Fp;

/// The root of the Tachyon polynomial accumulator.
///
/// This serves as the anchor for Tachyon transactions, analogous to
/// Orchard's `Anchor` but representing a polynomial commitment rather
/// than a Merkle root.
///
/// The accumulator root and tachygrams serve as public inputs to the
/// Ragu proof verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccumulatorRoot(Fp);

impl AccumulatorRoot {
    /// Creates a new accumulator root from a field element.
    pub fn from_field(value: Fp) -> Self {
        Self(value)
    }

    /// Returns the inner field element.
    pub fn inner(&self) -> Fp {
        self.0
    }

    /// Returns the byte representation of this accumulator root.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_repr()
    }

    /// Creates an accumulator root from bytes.
    ///
    /// Returns `None` if the bytes do not represent a valid field element.
    pub fn from_bytes(bytes: &[u8; 32]) -> Option<Self> {
        Fp::from_repr(*bytes).map(Self).into()
    }
}

impl From<Fp> for AccumulatorRoot {
    fn from(value: Fp) -> Self {
        Self(value)
    }
}

impl From<AccumulatorRoot> for Fp {
    fn from(root: AccumulatorRoot) -> Self {
        root.0
    }
}

impl Default for AccumulatorRoot {
    fn default() -> Self {
        Self(Fp::ZERO)
    }
}

/// A Tachyon polynomial accumulator.
///
/// The accumulator maintains a commitment to all tachygrams (note commitments
/// and nullifiers) in the Tachyon shielded pool.
///
/// ## Hash-Chain Model
///
/// The current stub uses a Poseidon hash chain defined recursively:
///
/// $$A_0 = H(\mathcal{D})$$
/// $$A_{i+1} = H(A_i \| t_i)$$
///
/// where:
/// - $H$ is the Poseidon hash function
/// - $\mathcal{D}$ is the domain separator ([`ACCUMULATOR_DOMAIN`](crate::primitives::ACCUMULATOR_DOMAIN))
/// - $t_i$ is the $i$-th tachygram
///
/// ## Polynomial Commitment (Future)
///
/// The full implementation will use KZG polynomial commitments, where the
/// accumulator root is a commitment to $p(X) = \prod_i (X - t_i)$ and membership
/// proofs are polynomial evaluation proofs at the queried point.
#[derive(Clone, Debug, Default)]
pub struct Accumulator {
    /// The current root of the accumulator.
    root: AccumulatorRoot,
    /// The number of tachygrams accumulated.
    count: u64,
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

    /// Returns the number of tachygrams in the accumulator.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Appends a tachygram to the accumulator.
    ///
    /// This is a stub that increments the count but does not perform
    /// the actual polynomial commitment update.
    pub fn append(&mut self, _tachygram: Tachygram) {
        self.count += 1;
        // TODO: Implement actual polynomial commitment update using Poseidon
        // self.root = hash(self.root, tachygram)
    }

    /// Appends multiple tachygrams to the accumulator in batch.
    pub fn append_batch(&mut self, tachygrams: impl IntoIterator<Item = Tachygram>) {
        for tachygram in tachygrams {
            self.append(tachygram);
        }
    }
}

/// A witness proving membership of a tachygram in the accumulator.
///
/// This witness can be used in a Ragu proof to demonstrate that a
/// particular note commitment or nullifier is part of the accumulated set.
#[derive(Clone, Debug)]
pub struct MembershipWitness {
    /// The tachygram being proven.
    pub tachygram: Tachygram,
    /// The accumulator root at the time of membership.
    pub root: AccumulatorRoot,
    // TODO: Add actual witness data for polynomial membership proof
}
