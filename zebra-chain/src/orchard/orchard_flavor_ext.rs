//! This module defines traits and structures for supporting the Orchard Shielded Protocol
//! for `V5` and `V6` versions of the transaction.
use std::fmt::Debug;

use serde::{de::DeserializeOwned, Serialize};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use orchard::{domain::OrchardDomainCommon, orchard_flavor};

use crate::{
    orchard::ValueCommitment,
    serialization::{ZcashDeserialize, ZcashSerialize},
};

#[cfg(feature = "tx-v6")]
use crate::orchard_zsa::{Burn, NoBurn};

use super::note;

#[cfg(not(any(test, feature = "proptest-impl")))]
pub trait TestArbitrary {}

#[cfg(not(any(test, feature = "proptest-impl")))]
impl<T> TestArbitrary for T {}

#[cfg(any(test, feature = "proptest-impl"))]
pub trait TestArbitrary: proptest::prelude::Arbitrary {}

#[cfg(any(test, feature = "proptest-impl"))]
impl<T: proptest::prelude::Arbitrary> TestArbitrary for T {}

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
        + TestArbitrary;

    /// Specifies the Orchard protocol flavor from `orchard` crate used by this implementation.
    type Flavor: orchard_flavor::OrchardFlavor;

    /// The size of the encrypted note for this protocol version.
    const ENCRYPTED_NOTE_SIZE: usize = Self::Flavor::ENC_CIPHERTEXT_SIZE;

    /// A type representing a burn field for this protocol version.
    #[cfg(feature = "tx-v6")]
    type BurnType: Clone
        + Debug
        + Default
        + ZcashDeserialize
        + ZcashSerialize
        + Into<ValueCommitment>
        + TestArbitrary;
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

impl OrchardFlavorExt for OrchardVanilla {
    type Flavor = orchard_flavor::OrchardVanilla;
    type EncryptedNote = note::EncryptedNote<{ Self::ENCRYPTED_NOTE_SIZE }>;

    #[cfg(feature = "tx-v6")]
    type BurnType = NoBurn;
}

#[cfg(feature = "tx-v6")]
impl OrchardFlavorExt for OrchardZSA {
    type Flavor = orchard_flavor::OrchardZSA;
    type EncryptedNote = note::EncryptedNote<{ Self::ENCRYPTED_NOTE_SIZE }>;

    type BurnType = Burn;
}
