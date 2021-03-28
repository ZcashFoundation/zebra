//! Orchard notes

#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use group::GroupEncoding;
use halo2::{arithmetic::FieldExt, pasta::pallas};
use rand_core::{CryptoRng, RngCore};

use crate::{
    amount::{Amount, NonNegative},
    transaction::Memo,
};

use super::{
    commitment::CommitmentRandomness,
    keys::{prf_expand, Diversifier, TransmissionKey},
    sinsemilla::extract_p,
};

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
mod ciphertexts;
mod nullifiers;

pub use ciphertexts::{EncryptedNote, WrappedNoteKey};
pub use nullifiers::Nullifier;

#[derive(Clone, Debug)]
pub struct SeedRandomness(pub(crate) [u8; 32]);

/// Used as input to PRF^nf as part of deriving the _nullifier_ of the _note_.
///
/// When creating a new note from spending an old note, the new note's _rho_ is
/// the _nullifier_ of the previous note. If creating a note from scratch (like
/// a miner reward), a dummy note is constructed, and its nullifier as the _rho_
/// for the actual output note. When creating a dummy note, its _rho_ is chosen
/// as a random Pallas point's x-coordinate.
///
/// https://zips.z.cash/protocol/nu5.pdf#orcharddummynotes
#[derive(Clone, Debug)]
pub struct Rho(pub(crate) pallas::Base);

impl From<Rho> for [u8; 32] {
    fn from(rho: Rho) -> Self {
        rho.0.to_bytes()
    }
}

impl Rho {
    pub fn new<T>(csprng: &mut T) -> Self
    where
        T: RngCore + CryptoRng,
    {
        let mut bytes = [0u8; 32];
        csprng.fill_bytes(&mut bytes);

        Self(extract_p(pallas::Point::from_bytes(&bytes).unwrap()))
    }
}

/// Additional randomness used in deriving the _nullifier_.
///
/// https://zips.z.cash/protocol/nu5.pdf#orchardsend
#[derive(Clone, Debug)]
pub struct Psi(pub(crate) pallas::Base);

impl From<Psi> for [u8; 32] {
    fn from(psi: Psi) -> Self {
        psi.0.to_bytes()
    }
}

impl From<SeedRandomness> for Psi {
    /// rcm = ToScalar^Orchard((PRF^expand_rseed ([9]))
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#orchardsend
    fn from(rseed: SeedRandomness) -> Self {
        Self(pallas::Base::from_bytes_wide(&prf_expand(
            rseed.0,
            vec![&[9]],
        )))
    }
}

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
    pub rho: Rho,
    /// Additional randomness used in deriving the _nullifier_.
    pub psi: Psi,
    /// A random _commitment trapdoor_ used to produce the associated note
    /// commitment.
    pub rcm: CommitmentRandomness,
    /// The note memo, after decryption.
    pub memo: Memo,
}
