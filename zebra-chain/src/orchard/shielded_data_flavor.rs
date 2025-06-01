//! This module defines traits and structures for supporting the Orchard Shielded Protocol
//! for `V5` and `V6` versions of the transaction.
use std::fmt::Debug;

use serde::{de::DeserializeOwned, Serialize};

use orchard::{domain::OrchardDomainCommon, orchard_flavor::OrchardFlavor};

pub use orchard::orchard_flavor::OrchardVanilla;

#[cfg(feature = "tx-v6")]
pub use orchard::{note::AssetBase, orchard_flavor::OrchardZSA, value::NoteValue};

use crate::serialization::{ZcashDeserialize, ZcashSerialize};

#[cfg(feature = "tx-v6")]
use crate::{
    orchard::ValueCommitment,
    orchard_zsa::{Burn, BurnItem, NoBurn},
};

use super::note;

// When testing or with the proptest-impl feature, enforce Arbitrary.
#[cfg(any(test, feature = "proptest-impl"))]
mod test_arbitrary {
    use proptest::prelude::Arbitrary;

    pub trait TestArbitrary: Arbitrary {}
    impl<T: Arbitrary> TestArbitrary for T {}
}

// Otherwise, no extra requirement.
#[cfg(not(any(test, feature = "proptest-impl")))]
mod test_arbitrary {
    pub trait TestArbitrary {}
    impl<T> TestArbitrary for T {}
}

/// A trait representing compile-time settings of ShieldedData of Orchard Shielded Protocol
/// used in the transactions `V5` and `V6`.
pub trait ShieldedDataFlavor: OrchardFlavor {
    /// A type representing an encrypted note for this protocol version.
    type EncryptedNote: Clone
        + Debug
        + PartialEq
        + Eq
        + DeserializeOwned
        + Serialize
        + ZcashDeserialize
        + ZcashSerialize
        + for<'a> TryFrom<&'a [u8], Error = std::array::TryFromSliceError>
        + test_arbitrary::TestArbitrary;

    /// A type representing a burn field for this protocol version.
    #[cfg(feature = "tx-v6")]
    // FIXME: try to get rid
    type BurnType: Clone
        + Debug
        + Default
        + ZcashDeserialize
        + ZcashSerialize
        + Into<ValueCommitment>
        + AsRef<[BurnItem]>
        + for<'a> From<&'a [(AssetBase, NoteValue)]>
        + test_arbitrary::TestArbitrary;
}

impl ShieldedDataFlavor for OrchardVanilla {
    type EncryptedNote = note::EncryptedNote<{ OrchardVanilla::ENC_CIPHERTEXT_SIZE }>;
    #[cfg(feature = "tx-v6")]
    type BurnType = NoBurn;
}

#[cfg(feature = "tx-v6")]
impl ShieldedDataFlavor for OrchardZSA {
    type EncryptedNote = note::EncryptedNote<{ OrchardZSA::ENC_CIPHERTEXT_SIZE }>;
    type BurnType = Burn;
}
