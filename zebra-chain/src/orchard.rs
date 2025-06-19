//! Orchard-related functionality.

#![warn(missing_docs)]

mod action;
mod address;
mod commitment;
mod note;
mod shielded_data_flavor;
mod sinsemilla;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;

pub mod keys;
pub mod shielded_data;
pub mod tree;

pub use action::Action;
pub use address::Address;
pub use commitment::{CommitmentRandomness, NoteCommitment, ValueCommitment};
pub use keys::Diversifier;
pub use note::{EncryptedNote, Note, Nullifier, WrappedNoteKey};
pub use shielded_data::{AuthorizedAction, Flags, ShieldedData};
pub use shielded_data_flavor::{OrchardVanilla, ShieldedDataFlavor};

pub(crate) use shielded_data::ActionCommon;

#[cfg(feature = "tx-v6")]
pub use shielded_data_flavor::OrchardZSA;
