//! Orchard notes

use group::{ff::PrimeField, GroupEncoding};
use halo2::pasta::pallas;
use rand_core::{CryptoRng, RngCore};

use crate::{
    amount::{Amount, NonNegative},
    error::{NoteError, RandError},
};

use super::{address::Address, sinsemilla::extract_p};

mod ciphertexts;
mod nullifiers;

pub use ciphertexts::{EncryptedNote, WrappedNoteKey};
pub use nullifiers::Nullifier;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

/// A random seed (rseed) used in the Orchard note creation.
#[derive(Clone, Copy, Debug)]
// At the moment this field is never read.
//
// TODO: consider replacing this code with the equivalent `orchard` crate code,
//       which is better tested.
#[allow(dead_code)]
pub struct SeedRandomness(pub(crate) [u8; 32]);

impl SeedRandomness {
    pub fn new<T>(csprng: &mut T) -> Result<Self, RandError>
    where
        T: RngCore + CryptoRng,
    {
        let mut bytes = [0u8; 32];
        csprng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| RandError::FillBytes)?;
        Ok(Self(bytes))
    }
}

/// Used as input to PRF^nf as part of deriving the _nullifier_ of the _note_.
///
/// When creating a new note from spending an old note, the new note's _rho_ is
/// the _nullifier_ of the previous note. If creating a note from scratch (like
/// a miner reward), a dummy note is constructed, and its nullifier as the _rho_
/// for the actual output note. When creating a dummy note, its _rho_ is chosen
/// as a random Pallas point's x-coordinate.
///
/// <https://zips.z.cash/protocol/nu5.pdf#orcharddummynotes>
#[derive(Clone, Debug)]
pub struct Rho(pub(crate) pallas::Base);

impl From<Rho> for [u8; 32] {
    fn from(rho: Rho) -> Self {
        rho.0.to_repr()
    }
}

impl From<Nullifier> for Rho {
    fn from(nf: Nullifier) -> Self {
        Self(nf.0)
    }
}

impl Rho {
    pub fn new<T>(csprng: &mut T) -> Result<Self, NoteError>
    where
        T: RngCore + CryptoRng,
    {
        let mut bytes = [0u8; 32];
        csprng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| NoteError::from(RandError::FillBytes))?;

        let possible_point = pallas::Point::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(extract_p(possible_point.unwrap())))
        } else {
            Err(NoteError::InvalidRho)
        }
    }
}

/// Additional randomness used in deriving the _nullifier_.
///
/// <https://zips.z.cash/protocol/nu5.pdf#orchardsend>
#[derive(Clone, Debug)]
pub struct Psi(pub(crate) pallas::Base);

impl From<Psi> for [u8; 32] {
    fn from(psi: Psi) -> Self {
        psi.0.to_repr()
    }
}

/// A Note represents that a value is spendable by the recipient who holds the
/// spending key corresponding to a given shielded payment address.
///
/// <https://zips.z.cash/protocol/protocol.pdf#notes>
#[derive(Clone, Debug)]
pub struct Note {
    /// The recipient's shielded payment address.
    pub address: Address,
    /// An integer representing the value of the _note_ in zatoshi.
    pub value: Amount<NonNegative>,
    /// Used as input to PRF^nfOrchard_nk as part of deriving the _nullifier_ of
    /// the _note_.
    pub rho: Rho,
    /// 32 random bytes from which _rcm_, _psi_, and the _ephemeral private key_
    /// are derived.
    pub rseed: SeedRandomness,
}

impl Note {
    /// Create an Orchard _note_, by choosing 32 uniformly random bytes for
    /// rseed.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#notes>
    pub fn new<T>(
        csprng: &mut T,
        address: Address,
        value: Amount<NonNegative>,
        nf_old: Nullifier,
    ) -> Result<Self, RandError>
    where
        T: RngCore + CryptoRng,
    {
        Ok(Self {
            address,
            value,
            rho: nf_old.into(),
            rseed: SeedRandomness::new(csprng)?,
        })
    }
}
