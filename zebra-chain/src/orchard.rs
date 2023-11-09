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

mod tx_version;

#[cfg(feature = "tx-v6")]
pub mod zsa;

#[cfg(feature = "tx-v6")]
pub mod burn;

#[cfg(feature = "tx-v6")]
pub mod issuance;

pub use action::Action;
pub use address::Address;
pub use commitment::{CommitmentRandomness, NoteCommitment, ValueCommitment};
pub use keys::Diversifier;
pub use note::{EncryptedNote, Note, Nullifier, WrappedNoteKey};
pub use shielded_data::{ActionRef, AuthorizedAction, Flags, ShieldedData};

pub use tx_version::{TxV5, TxVersion};

#[cfg(feature = "tx-v6")]
pub use tx_version::TxV6;

#[cfg(any(test, feature = "proptest-impl"))]
pub use tx_version::ENCRYPTED_NOTE_SIZE_V5;

#[cfg(feature = "tx-v6")]
pub use burn::BurnItem;
