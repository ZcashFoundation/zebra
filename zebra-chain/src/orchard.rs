//! Orchard-related functionality.

#![warn(missing_docs)]

mod action;
mod address;
mod commitment;
mod note;
mod sinsemilla;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;

pub mod keys;
pub mod shielded_data;
pub mod tree;

mod orchard_variant;

pub use action::Action;
pub use address::Address;
pub use commitment::{CommitmentRandomness, NoteCommitment, ValueCommitment};
pub use keys::Diversifier;
pub use note::{EncryptedNote, Note, Nullifier, WrappedNoteKey};
pub use shielded_data::{ActionRef, AuthorizedAction, Flags, ShieldedData};

pub use orchard_variant::{OrchardVariant, TxV5};

#[cfg(feature = "tx-v6")]
pub use orchard_variant::TxV6;

#[cfg(any(test, feature = "proptest-impl"))]
pub use orchard_variant::ENCRYPTED_NOTE_SIZE_V5;
