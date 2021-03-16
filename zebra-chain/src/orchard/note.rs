//! Orchard notes

#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use halo2::pasta::pallas;

use crate::{
    amount::{Amount, NonNegative},
    transaction::Memo,
};

use super::{
    commitment::CommitmentRandomness,
    keys::{Diversifier, TransmissionKey},
};

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
mod ciphertexts;
mod nullifiers;

pub use ciphertexts::{EncryptedNote, WrappedNoteKey};
pub use nullifiers::Nullifier;

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
#[derive(Clone, Debug)]
pub struct Note {
    /// The _diversifer_ of the recipient’s _shielded payment address_.
    pub diversifier: Diversifier,
    /// The _diversified transmission key_ of the recipient’s shielded
    /// payment address.
    pub transmission_key: TransmissionKey,
    /// An integer representing the value of the _note_ in zatoshi.
    pub value: Amount<NonNegative>,
    /// Used as input to PRF^nf as part of deriving the _nullifier_ of the _note_.
    pub rho: pallas::Base, // XXX: refine type?
    /// Additional randomness used in deriving the _nullifier_.
    pub psi: pallas::Base, // XXX: refine type
    /// A random _commitment trapdoor_ used to produce the associated note
    /// commitment.
    pub rcm: CommitmentRandomness,
    /// The note memo, after decryption.
    pub memo: Memo,
}
