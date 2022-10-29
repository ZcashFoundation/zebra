//! Sprout notes

mod ciphertexts;
mod mac;
mod nullifiers;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

use crate::{
    amount::{Amount, NonNegative},
    transaction::Memo,
};

use super::{commitment::CommitmentRandomness, keys::PayingKey};

pub use mac::Mac;

pub use ciphertexts::EncryptedNote;

pub use nullifiers::{Nullifier, NullifierSeed};

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
///
/// <https://zips.z.cash/protocol/protocol.pdf#notes>
#[derive(Clone, Debug)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Note {
    /// The paying key of the recipient's shielded payment address
    pub paying_key: PayingKey,
    /// An integer representing the value of the note in zatoshi (1 ZEC
    /// = 10^8 zatoshi)
    pub value: Amount<NonNegative>,
    /// Input to PRF^nf to derive the nullifier of the note
    pub rho: NullifierSeed,
    /// A random commitment trapdoor
    pub rcm: CommitmentRandomness,
    /// The note memo, after decryption
    pub memo: Memo,
}
