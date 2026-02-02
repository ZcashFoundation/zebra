//! Tachyon-related functionality.
//!
//! Tachyon is a scaling solution for Zcash that introduces:
//! - Tachygrams: Unified 32-byte blobs (nullifiers or note commitments)
//! - Tachyactions: Simplified actions with epoch-flavored nullifiers
//! - Aggregate proof transactions via Ragu PCD
//! - Out-of-band payment distribution (no ciphertexts on-chain)
//!
//! ## Aggregate Transaction Model
//!
//! Tachyon uses an aggregate proof model:
//! - **Aggregate transactions** contain an [`AggregateProof`] covering multiple tachyon txs
//! - **Regular tachyon transactions** reference an aggregate by [`transaction::Hash`]
//! - Multiple aggregates may exist per block
//!
//! ## Type Re-exports
//!
//! This module re-exports core types from the [`tachyon`] crate:
//!
//! - [`Epoch`] - epoch identifier for nullifier flavoring
//! - [`Nullifier`] - nullifier value (just the nf, without epoch)
//! - [`Accumulator`], [`AccumulatorRoot`], [`MembershipWitness`] - polynomial accumulator
//!
//! ## Blockchain-Specific Types
//!
//! These types provide [`ZcashSerialize`]/[`ZcashDeserialize`] for blockchain storage:
//!
//! - [`ShieldedData`] - regular tachyon transaction data (references an aggregate)
//! - [`AggregateData`] - aggregate transaction data (contains the proof)
//! - [`FlavoredNullifier`] - bundles a [`Nullifier`] with its [`Epoch`]
//! - [`NoteCommitment`] - full curve point (use [`tachyon::NoteCommitment`] for x-coordinate)
//! - [`ValueCommitment`] - homomorphic commitment with Add/Sub/Sum
//! - [`Tachygram`] - 32-byte blob for accumulator entries
//! - [`Anchor`](accumulator::Anchor) - serializable accumulator root

#![warn(missing_docs)]

mod action;
mod commitment;
mod nullifier;
mod proof;
mod tachygram;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

pub mod accumulator;
pub mod shielded_data;

// Re-export core types from tachyon crate
pub use tachyon::{Accumulator, AccumulatorRoot, Epoch, MembershipWitness, Nullifier};

// Blockchain-specific types with ZcashSerialize/Deserialize
pub use action::{AuthorizedTachyaction, Tachyaction};
pub use commitment::{NoteCommitment, ValueCommitment};
pub use nullifier::FlavoredNullifier;
pub use proof::AggregateProof;
pub use shielded_data::{AggregateData, Flags, ShieldedData};
pub use tachygram::Tachygram;
