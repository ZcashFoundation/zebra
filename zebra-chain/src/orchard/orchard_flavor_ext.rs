//! This module defines traits and structures for supporting the Orchard Shielded Protocol
//! for `V5` and `V6` versions of the transaction.
use std::{fmt::Debug, io};

use serde::{de::DeserializeOwned, Serialize};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use orchard::{note_encryption::OrchardDomainCommon, orchard_flavor};

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

use super::note;

#[cfg(feature = "tx-v6")]
use crate::orchard_zsa::burn::BurnItem;

#[cfg(not(any(test, feature = "proptest-impl")))]
pub trait EncryptedNoteTest {}

#[cfg(not(any(test, feature = "proptest-impl")))]
impl<T> EncryptedNoteTest for T {}

#[cfg(any(test, feature = "proptest-impl"))]
pub trait EncryptedNoteTest: proptest::prelude::Arbitrary {}

#[cfg(any(test, feature = "proptest-impl"))]
impl<T: proptest::prelude::Arbitrary> EncryptedNoteTest for T {}

/// A trait representing compile-time settings of Orchard Shielded Protocol used in
/// the transactions `V5` and `V6`.
pub trait OrchardFlavorExt: Clone + Debug {
    /// A type representing an encrypted note for this protocol version.
    type EncryptedNote: Clone
        + Debug
        + PartialEq
        + Eq
        + DeserializeOwned
        + Serialize
        + ZcashDeserialize
        + ZcashSerialize
        + EncryptedNoteTest;

    /// FIXME: add doc
    type Flavor: orchard_flavor::OrchardFlavor;

    /// The size of the encrypted note for this protocol version.
    const ENCRYPTED_NOTE_SIZE: usize = Self::Flavor::ENC_CIPHERTEXT_SIZE;

    /// A type representing a burn field for this protocol version.
    // FIXME: add cfg tx-v6 here?
    type BurnType: Clone + Debug + Default + ZcashDeserialize + ZcashSerialize;
}

/// A structure representing a tag for Orchard protocol variant used for the transaction version `V5`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct OrchardVanilla;

/// A structure representing a tag for Orchard protocol variant used for the transaction version `V6`
/// (which ZSA features support).
#[cfg(feature = "tx-v6")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct OrchardZSA;

/// A special marker type indicating the absence of a burn field in Orchard ShieldedData for `V5` transactions.
/// Useful for unifying ShieldedData serialization and deserialization implementations across various
/// Orchard protocol variants (i.e. various transaction versions).
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NoBurn;

impl ZcashSerialize for NoBurn {
    fn zcash_serialize<W: io::Write>(&self, mut _writer: W) -> Result<(), io::Error> {
        Ok(())
    }
}

impl ZcashDeserialize for NoBurn {
    fn zcash_deserialize<R: io::Read>(mut _reader: R) -> Result<Self, SerializationError> {
        Ok(Self)
    }
}

impl OrchardFlavorExt for OrchardVanilla {
    type Flavor = orchard_flavor::OrchardVanilla;
    type EncryptedNote = note::EncryptedNote<{ Self::ENCRYPTED_NOTE_SIZE }>;
    type BurnType = NoBurn;
}

#[cfg(feature = "tx-v6")]
impl OrchardFlavorExt for OrchardZSA {
    type Flavor = orchard_flavor::OrchardZSA;
    type EncryptedNote = note::EncryptedNote<{ Self::ENCRYPTED_NOTE_SIZE }>;
    type BurnType = Vec<BurnItem>;
}
