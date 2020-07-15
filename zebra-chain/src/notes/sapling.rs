#![allow(clippy::unit_arg)]
#![allow(dead_code)]

#[cfg(test)]
mod arbitrary;
mod ciphertexts;
mod commitments;
mod nullifiers;

use crate::{
    keys::sapling::{Diversifier, TransmissionKey},
    notes::memo::Memo,
    types::amount::{Amount, NonNegative},
};

pub use ciphertexts::{EncryptedCiphertext, OutCiphertext};
pub use commitments::{CommitmentRandomness, NoteCommitment, ValueCommitment};
pub use nullifiers::Nullifier;

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
pub struct Note {
    diversifier: Diversifier,
    transmission_key: TransmissionKey,
    value: Amount<NonNegative>,
    rcm: CommitmentRandomness,
}

impl Note {
    /// Construct a “windowed” Pedersen commitment by reusing a
    /// Perderson hash constructon, and adding a randomized point on
    /// the Jubjub curve.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
    pub fn commit(&self) -> NoteCommitment {
        unimplemented!()
    }
}

/// The decrypted form of encrypted Sapling notes on the blockchain.
pub struct NotePlaintext {
    diversifier: Diversifier,
    value: Amount<NonNegative>,
    rcm: CommitmentRandomness,
    memo: Memo,
}
