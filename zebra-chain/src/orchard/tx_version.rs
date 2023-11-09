//! This module defines traits and structures for supporting the Orchard Shielded Protocol for `V5` and `V6` versions of the transaction.
use std::fmt::Debug;

use serde::{de::DeserializeOwned, Serialize};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use crate::serialization::{ZcashDeserialize, ZcashSerialize};

use super::note;

#[cfg(feature = "tx-v6")]
use crate::orchard::burn::BurnItem;

/// The size of the encrypted note for the Orchard ShieldedData of `V5` transactions.
pub const ENCRYPTED_NOTE_SIZE_V5: usize = 580;

/// The size of the encrypted note for the Orchard ShieldedData of `V6` transactions.
#[cfg(feature = "tx-v6")]
pub const ENCRYPTED_NOTE_SIZE_V6: usize = orchard_zsa::note_encryption_v3::ENC_CIPHERTEXT_SIZE_V3;

/// A trait representing a version of the transaction with Orchard Shielded Protocol support.
pub trait TxVersion: Clone + Debug {
    /// The size of the encrypted note for this protocol version.
    const ENCRYPTED_NOTE_SIZE: usize;

    /// A type representing an encrypted note for this protocol version.
    type EncryptedNote: Clone
        + Debug
        + PartialEq
        + Eq
        + DeserializeOwned
        + Serialize
        + ZcashDeserialize
        + ZcashSerialize;

    /// A type representing a burn field for this protocol version.
    type BurnType: Debug + Default + ZcashDeserialize + ZcashSerialize;
}

/// A structure representing a tag for the transaction version `V5` with Orchard protocol support.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct TxV5;

/// A structure representing a tag for the transaction version `V6` with Orchard protocol support.
#[cfg(feature = "tx-v6")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct TxV6;

impl TxVersion for TxV5 {
    const ENCRYPTED_NOTE_SIZE: usize = ENCRYPTED_NOTE_SIZE_V5;
    type EncryptedNote = note::EncryptedNote<ENCRYPTED_NOTE_SIZE_V5>;
    // TODO: FIXME: consider using a custom union type instead.
    type BurnType = ();
}

#[cfg(feature = "tx-v6")]
impl TxVersion for TxV6 {
    const ENCRYPTED_NOTE_SIZE: usize = ENCRYPTED_NOTE_SIZE_V6;
    type EncryptedNote = note::EncryptedNote<ENCRYPTED_NOTE_SIZE_V6>;
    type BurnType = Vec<BurnItem>;
}
