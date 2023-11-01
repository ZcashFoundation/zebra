//! This module defines traits and structures for supporting the Orchard Shielded Protocol for `V5` and `V6` versions of the transaction.
use std::fmt::Debug;

use serde::{de::DeserializeOwned, Serialize};

use crate::serialization::{ZcashDeserialize, ZcashSerialize};

use super::note;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::prelude::Arbitrary;

/// The size of the encrypted note for the Orchard ShieldedData of `V5` transactions.
pub const ENCRYPTED_NOTE_SIZE_V5: usize = 580;

/// The size of the encrypted note for the Orchard ShieldedData of `V6` transactions.
#[cfg(feature = "tx-v6")]
pub const ENCRYPTED_NOTE_SIZE_V6: usize = orchard_zsa::note_encryption_v3::ENC_CIPHERTEXT_SIZE_V3;

/// For test builds ('cargo test'), the Arbitrary trait is required for the EncryptedNote associated
/// type of the TxVersion trait.
#[cfg(any(test, feature = "proptest-impl"))]
pub trait PropTest: Arbitrary {}

/// An empty trait used in regular (non-test) builds, as Arbitrary is only needed for 'cargo test'.
// FIXME: consider using another way to provide the Arbitrary constraint for tests.
#[cfg(not(any(test, feature = "proptest-impl")))]
pub trait PropTest {}
impl<const N: usize> PropTest for note::EncryptedNote<N> {}

/// A trait representing a version of the transaction with Orchard Shielded Protocol support.
pub trait TxVersion: Clone + Debug {
    /// The size of the encrypted note for this protocol version.
    const ENCRYPTED_NOTE_SIZE: usize;

    /// Indicates whether the transaction contains a burn field in the Orchard ShieldedData.
    const HAS_BURN: bool;

    /// A type representing an encrypted note for this protocol version.
    type EncryptedNote: Clone
        + Debug
        + PartialEq
        + Eq
        + DeserializeOwned
        + Serialize
        + ZcashDeserialize
        + ZcashSerialize
        + PropTest;
}

/// A structure representing a tag for the transaction version `V5` with Orchard protocol support.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct V5;

/// A structure representing a tag for the transaction version `V6` with Orchard protocol support.
#[cfg(feature = "tx-v6")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct V6;

impl TxVersion for V5 {
    const ENCRYPTED_NOTE_SIZE: usize = ENCRYPTED_NOTE_SIZE_V5;
    const HAS_BURN: bool = false;
    type EncryptedNote = note::EncryptedNote<ENCRYPTED_NOTE_SIZE_V5>;
}

#[cfg(feature = "tx-v6")]
impl TxVersion for V6 {
    const ENCRYPTED_NOTE_SIZE: usize = ENCRYPTED_NOTE_SIZE_V6;
    const HAS_BURN: bool = true;
    type EncryptedNote = note::EncryptedNote<ENCRYPTED_NOTE_SIZE_V6>;
}
