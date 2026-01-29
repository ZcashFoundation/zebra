//! Tachyon-related functionality.
//!
//! Tachyon is a scaling solution for Zcash that introduces:
//! - Tachygrams: Unified 32-byte blobs (nullifiers or note commitments)
//! - Tachyactions: Simplified actions with epoch-flavored nullifiers
//! - Block-level proof aggregation via Ragu PCD
//! - Out-of-band payment distribution (no ciphertexts on-chain)

#![warn(missing_docs)]

mod action;
mod commitment;
mod epoch;
mod nullifier;
mod proof;
mod tachygram;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

pub mod accumulator;
pub mod shielded_data;

pub use action::{AuthorizedTachyaction, Tachyaction};
pub use commitment::{NoteCommitment, ValueCommitment};
pub use epoch::EpochId;
pub use nullifier::Nullifier;
pub use proof::{RaguBlockProof, TransactionProof};
pub use shielded_data::{Flags, ShieldedData};
pub use tachygram::Tachygram;
