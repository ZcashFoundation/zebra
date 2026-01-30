//! # tachyon
//!
//! The Tachyon shielded transaction protocol.
//!
//! Tachyon is a scaling solution for Zcash that enables:
//! - **Proof Aggregation**: Multiple Halo proofs aggregated into a single Ragu proof per block
//! - **Oblivious Synchronization**: Wallets can outsource sync to untrusted services
//! - **Polynomial Accumulators**: Unified tracking of commitments and nullifiers via tachygrams
//!
//! ## Status
//!
//! This crate is currently a stub for progressive development. Types are
//! placeholders and will be implemented as the protocol specification matures.
//!
//! ## Nomenclature
//!
//! All types in the `tachyon` crate, unless otherwise specified, are Tachyon-specific
//! types. For example, [`Address`] is a Tachyon payment address, and [`Tachygram`]
//! is a unified commitment/nullifier representation unique to Tachyon.

#![cfg_attr(docsrs, feature(doc_cfg))]
// Temporary until we have more of the crate implemented.
#![allow(dead_code)]
// Catch documentation errors caused by code changes.
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_debug_implementations)]
#![deny(missing_docs)]
#![deny(unsafe_code)]

pub mod accumulator;
mod action;
mod address;
pub mod bundle;
pub mod keys;
pub mod note;
pub mod primitives;
pub mod tachygram;
pub mod value;

pub use accumulator::{Accumulator, AccumulatorRoot, MembershipWitness};
pub use action::Action;
pub use address::Address;
pub use bundle::Bundle;
pub use keys::{
    ConstrainedNullifierKey, FullViewingKey, IncomingViewingKey, NullifierKey, SpendingKey,
};
pub use note::{Epoch, Note, NoteCommitment, Nullifier, NullifierTrapdoor};
pub use tachygram::Tachygram;
pub use value::ValueCommitment;
