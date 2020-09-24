//! Sprout-related functionality.

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
mod joinsplit;

// XXX clean up these modules

pub mod address;
pub mod commitment;
pub mod keys;
pub mod note;
pub mod tree;

pub use joinsplit::JoinSplit;
pub use note::{EncryptedNote, Note, Nullifier};
