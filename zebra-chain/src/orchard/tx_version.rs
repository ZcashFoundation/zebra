//! This module defines traits and structures for supporting the Orchard Shielded Protocol for `V5` and `V6` versions of the transaction.
use std::fmt::Debug;

use serde::{de::DeserializeOwned, Serialize};

use crate::serialization::{ZcashDeserialize, ZcashSerialize};

use super::note;

/// The size of the encrypted note for the Orchard ShieldedData of `V5` transactions.
pub const ENCRYPTED_NOTE_SIZE_V5: usize = 580;

/// The size of the encrypted note for the Orchard ShieldedData of `V6` transactions.
#[cfg(feature = "tx-v6")]
pub const ENCRYPTED_NOTE_SIZE_V6: usize = orchard_zsa::note_encryption_v3::ENC_CIPHERTEXT_SIZE_V3;

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
        + ZcashSerialize;
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
