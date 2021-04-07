//! Sapling-related functionality.

mod address;
#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
mod commitment;
mod note;
mod output;
mod spend;
#[cfg(test)]
mod tests;

// XXX clean up these modules

pub mod keys;
pub mod shielded_data;
pub mod tree;

pub use address::Address;
pub use commitment::{CommitmentRandomness, NoteCommitment, ValueCommitment};
pub use keys::Diversifier;
pub use note::{EncryptedNote, Note, Nullifier, WrappedNoteKey};
pub use output::Output;
pub use shielded_data::{
    AnchorVariant, FieldNotPresent, PerSpendAnchor, SharedAnchor, ShieldedData,
};
pub use spend::Spend;
