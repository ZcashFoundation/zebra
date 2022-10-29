//! Sapling notes

mod ciphertexts;
mod nullifiers;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

use crate::{
    amount::{Amount, NonNegative},
    transaction::Memo,
};

use super::{
    commitment::CommitmentRandomness,
    keys::{Diversifier, TransmissionKey},
};

pub use ciphertexts::{EncryptedNote, WrappedNoteKey};

pub use nullifiers::Nullifier;

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
#[derive(Clone, Debug)]
pub struct Note {
    /// The diversifer of the recipient's shielded payment address.
    pub diversifier: Diversifier,
    /// The diversified transmission key of the recipient's shielded
    /// payment address.
    pub transmission_key: TransmissionKey,
    /// An integer representing the value of the note in zatoshi.
    pub value: Amount<NonNegative>,
    /// A random commitment trapdoor used to produce the associated
    /// note commitment.
    pub rcm: CommitmentRandomness,
    /// The note memo, after decryption
    pub memo: Memo,
}
