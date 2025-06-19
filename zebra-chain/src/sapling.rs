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

mod commitment;
mod note;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;

pub mod keys;
pub mod output;
pub mod shielded_data;
pub mod spend;
pub mod tree;

pub use commitment::{CommitmentRandomness, NotSmallOrderValueCommitment, ValueCommitment};
pub use keys::Diversifier;
pub use note::{EncryptedNote, Note, Nullifier, WrappedNoteKey};
pub use output::{Output, OutputInTransactionV4, OutputPrefixInTransactionV5};
pub use shielded_data::{
    AnchorVariant, FieldNotPresent, PerSpendAnchor, SharedAnchor, ShieldedData, TransferData,
};
pub use spend::{Spend, SpendPrefixInTransactionV5};
