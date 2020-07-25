//! Sprout notes

#![allow(clippy::unit_arg)]
#![allow(dead_code)]

#[cfg(test)]
mod arbitrary;
mod ciphertexts;
mod commitments;
mod nullifiers;

use sha2::{Digest, Sha256};

use crate::{
    keys::sprout::PayingKey,
    notes::memo::Memo,
    types::amount::{Amount, NonNegative},
};

pub use ciphertexts::EncryptedCiphertext;
pub use commitments::{CommitmentRandomness, NoteCommitment};
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
    paying_key: PayingKey,
    /// An integer representing the value of the note in zatoshi (1 ZEC
    /// = 10^8 zatoshi)
    value: Amount<NonNegative>,
    /// Input to PRF^nf to derive the nullifier of the note
    rho: NullifierSeed,
    /// A random commitment trapdoor
    rcm: CommitmentRandomness,
}

impl Note {
    /// NoteCommit_rcm^Sprout(a_pk, v, rho)
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretesproutnotecommit
    pub fn commit(&self) -> NoteCommitment {
        let leading_byte: u8 = 0xB0;
        let mut hasher = Sha256::default();
        hasher.input([leading_byte]);
        hasher.input(self.paying_key);
        hasher.input(self.value.to_bytes());
        hasher.input(self.rho);
        hasher.input(self.rcm);
        NoteCommitment(hasher.result().into())
    }
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
