//! Sprout notes

#![allow(clippy::unit_arg)]
#![allow(dead_code)]

#[cfg(test)]
mod arbitrary;
mod ciphertexts;
mod nullifiers;

use crate::{
    commitments::sprout::CommitmentRandomness,
    keys::sprout::PayingKey,
    notes::memo::Memo,
    types::amount::{Amount, NonNegative},
};

pub use ciphertexts::EncryptedCiphertext;

pub use nullifiers::{Nullifier, NullifierSeed};

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
///
/// https://zips.z.cash/protocol/protocol.pdf#notes
#[derive(Clone, Copy, Debug)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Note {
    /// The paying key of the recipient’s shielded payment address
    pub paying_key: PayingKey,
    /// An integer representing the value of the note in zatoshi (1 ZEC
    /// = 10^8 zatoshi)
    pub value: Amount<NonNegative>,
    /// Input to PRF^nf to derive the nullifier of the note
    pub rho: NullifierSeed,
    /// A random commitment trapdoor
    pub rcm: CommitmentRandomness,
}

/// The decrypted form of encrypted Sprout notes on the blockchain.
#[derive(Clone, Debug)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct NotePlaintext {
    value: Amount<NonNegative>,
    rho: NullifierSeed,
    rcm: CommitmentRandomness,
    memo: Memo,
}
