//! Low-level cryptographic primitives for Tachyon.
//!
//! This module provides the fundamental cryptographic building blocks used
//! throughout the Tachyon protocol, built on top of the Ragu proof system
//! and Pasta curves.
//!
//! ## Field Elements
//!
//! Tachyon uses the Pallas curve's base field $\mathbb{F}_p$ as its primary computation
//! field, consistent with the Orchard protocol. The scalar field $\mathbb{F}_q$ is used
//! for scalar operations on the Vesta curve.
//!
//! ## Poseidon Hash
//!
//! The Poseidon algebraic hash function is used for:
//! - Nullifier derivation via the GGM Tree PRF
//! - Note commitments
//! - Accumulator updates

use pasta_curves::pallas;

/// Re-export of the Pallas base field element type $\mathbb{F}_p$.
///
/// This is the primary field used for Tachyon computations.
pub type Fp = pallas::Base;

/// Re-export of the Pallas scalar field element type $\mathbb{F}_q$.
pub type Fq = pallas::Scalar;

/// Re-export of the Pallas curve affine point type.
pub type PallasPoint = pallas::Affine;

/// The Pasta curve cycle used for Tachyon proofs.
///
/// This re-exports the `Pasta` type from ragu_pasta, which provides
/// the complete curve cycle implementation including generators and
/// Poseidon parameters.
//pub use ragu_pasta::Pasta;

/// Domain separator for Tachyon nullifier derivation.
///
/// Used in the GGM Tree PRF construction to domain-separate
/// nullifier computations from other hash uses.
pub const NULLIFIER_DOMAIN: &[u8] = b"Tachyon_Nullifier";

/// Domain separator for Tachyon note commitments.
pub const NOTE_COMMITMENT_DOMAIN: &[u8] = b"Tachyon_NoteCommit";

/// Domain separator for Tachyon value commitments.
pub const VALUE_COMMITMENT_DOMAIN: &[u8] = b"Tachyon_ValueCommit";

/// Domain separator for the polynomial accumulator.
pub const ACCUMULATOR_DOMAIN: &[u8] = b"Tachyon_Accumulator";
