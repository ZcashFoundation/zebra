//! Sapling-related functionality.
//!
//! These data structures enforce the *structural validity* of Sapling-related
//! consensus-critical objects.
//!
//! **Consensus rule**:
//!
//! These data structures ensure that [ZIP-216](https://zips.z.cash/zip-0216),
//! canonical Jubjub point encodings, are enforced everywhere where Jubjub
//! points occur, and non-canonical point encodings are rejected. This is
//! enforced by the jubjub crate, which is also used by the redjubjub crate.

mod address;
#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
mod commitment;
mod note;
#[cfg(test)]
mod tests;

// XXX clean up these modules

pub mod keys;
pub mod output;
pub mod shielded_data;
pub mod spend;
pub mod tree;

pub use address::Address;
pub use commitment::{CommitmentRandomness, NoteCommitment, ValueCommitment};
pub use keys::Diversifier;
pub use note::{EncryptedNote, Note, Nullifier, WrappedNoteKey};
pub use output::{Output, OutputInTransactionV4, OutputPrefixInTransactionV5};
pub use shielded_data::{
    AnchorVariant, FieldNotPresent, PerSpendAnchor, SharedAnchor, ShieldedData, TransferData,
};
pub use spend::{Spend, SpendPrefixInTransactionV5};
