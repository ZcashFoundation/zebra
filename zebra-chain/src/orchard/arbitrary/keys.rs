//! Test-only Orchard key types.
//!
//! These key types have known bugs, or depend on keys with known bugs.
//! They can't be used in production until the TODOs are fixed.
//!
//! Production orchard key types are in the [`orchard::keys`] module.
//! Some other unused key types are not implemented.
//!
//! <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>

use std::fmt;

use bitvec::{
    prelude::{BitArray, Lsb0},
    vec::BitVec,
};
use group::{
    ff::{Field, PrimeField},
    GroupEncoding,
};
use halo2::pasta::pallas;
use rand::{CryptoRng, RngCore};
use subtle::{Choice, ConstantTimeEq};

use crate::{
    orchard::{
        keys::*,
        sinsemilla::{extract_p, sinsemilla_short_commit},
    },
    parameters::Network,
    primitives::redpallas,
};

#[cfg(test)]
mod tests;

#[allow(clippy::fallible_impl_from)]
impl From<[u8; 32]> for SpendValidatingKey {
    fn from(bytes: [u8; 32]) -> Self {
        // TODO: turn this unwrap() into a TryFrom error
        Self(redpallas::VerificationKey::try_from(bytes).unwrap())
    }
}

/// An _incoming viewing key_, as described in [protocol specification
/// §4.2.3][ps].
///
/// Used to decrypt incoming notes without spending them.
///
/// [ps]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone)]
pub struct IncomingViewingKey {
    dk: DiversifierKey,

    // TODO: refine type, so that IncomingViewingkey.ivk cannot be 0
    ivk: pallas::Scalar,
}

impl ConstantTimeEq for IncomingViewingKey {
    /// Check whether two `IncomingViewingKey`s are equal, runtime independent
    /// of the value of the secret.
    ///
    /// # Note
    ///
    /// This function should execute in time independent of the `dk` and `ivk` values.
    fn ct_eq(&self, other: &Self) -> Choice {
        <[u8; 64]>::from(*self).ct_eq(&<[u8; 64]>::from(*other))
    }
}

impl fmt::Debug for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("IncomingViewingKey")
            .field("dk", &self.dk)
            .field("ivk", &self.ivk)
            .finish()
    }
}

impl fmt::Display for IncomingViewingKey {
    /// The _raw encoding_ of an **Orchard** _incoming viewing key_.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#orchardfullviewingkeyencoding>
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&hex::encode(<[u8; 64]>::from(*self)))
    }
}

impl Eq for IncomingViewingKey {}

impl From<IncomingViewingKey> for [u8; 64] {
    fn from(ivk: IncomingViewingKey) -> [u8; 64] {
        let mut bytes = [0u8; 64];

        bytes[..32].copy_from_slice(&<[u8; 32]>::from(ivk.dk));
        bytes[32..].copy_from_slice(&<[u8; 32]>::from(ivk.ivk));

        bytes
    }
}

impl TryFrom<[u8; 64]> for IncomingViewingKey {
    type Error = &'static str;

    /// Convert an array of bytes into a [`IncomingViewingKey`].
    ///
    /// Returns an error if the encoding is malformed or if it [encodes the scalar additive
    /// identity, 0][1].
    ///
    /// > ivk MUST be in the range {1 .. 𝑞P - 1}
    ///
    /// [1]: https://zips.z.cash/protocol/protocol.pdf#orchardinviewingkeyencoding
    fn try_from(bytes: [u8; 64]) -> Result<Self, Self::Error> {
        let mut dk_bytes = [0u8; 32];
        dk_bytes.copy_from_slice(&bytes[..32]);
        let dk = DiversifierKey::from(dk_bytes);

        let mut ivk_bytes = [0u8; 32];
        ivk_bytes.copy_from_slice(&bytes[32..]);

        let possible_scalar = pallas::Scalar::from_repr(ivk_bytes);

        if possible_scalar.is_some().into() {
            let scalar = possible_scalar.unwrap();
            if scalar.is_zero().into() {
                Err("pallas::Scalar value for Orchard IncomingViewingKey is 0")
            } else {
                Ok(Self {
                    dk,
                    ivk: possible_scalar.unwrap(),
                })
            }
        } else {
            Err("Invalid pallas::Scalar value for Orchard IncomingViewingKey")
        }
    }
}

impl TryFrom<FullViewingKey> for IncomingViewingKey {
    type Error = &'static str;

    /// Commit^ivk_rivk(ak, nk) :=
    ///     SinsemillaShortCommit_rcm(︁
    ///        "z.cash:Orchard-CommitIvk",
    ///        I2LEBSP_l^Orchard_base(ak) || I2LEBSP_l^Orchard_base(nk)︁
    ///     ) mod r_P
    ///
    /// <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>
    /// <https://zips.z.cash/protocol/nu5.pdf#concreteprfs>
    #[allow(non_snake_case)]
    #[allow(clippy::unwrap_in_result)]
    fn try_from(fvk: FullViewingKey) -> Result<Self, Self::Error> {
        let mut M: BitVec<u8, Lsb0> = BitVec::new();

        // I2LEBSP_l^Orchard_base(ak)︁
        //
        // Converting a Point to bytes and back is infalliable.
        let ak_bytes =
            extract_p(pallas::Point::from_bytes(&fvk.spend_validating_key.into()).unwrap())
                .to_repr();
        M.extend_from_bitslice(&BitArray::<_, Lsb0>::from(ak_bytes)[0..255]);

        // I2LEBSP_l^Orchard_base(nk)︁
        let nk_bytes: [u8; 32] = fvk.nullifier_deriving_key.into();
        M.extend_from_bitslice(&BitArray::<_, Lsb0>::from(nk_bytes)[0..255]);

        // Commit^ivk_rivk
        // rivk needs to be 255 bits long
        let commit_x = sinsemilla_short_commit(
            fvk.ivk_commit_randomness.into(),
            b"z.cash:Orchard-CommitIvk",
            &M,
        )
        .expect("deriving orchard commit^ivk should not output ⊥ ");

        let ivk_ctoption = pallas::Scalar::from_repr(commit_x.into());

        // if ivk ∈ {0, ⊥}, discard this key

        // [`Scalar::is_zero()`] is constant-time under the hood, and ivk is mod r_P
        if ivk_ctoption.is_some().into() && !<bool>::from(ivk_ctoption.unwrap().is_zero()) {
            Ok(Self {
                dk: fvk.into(),
                ivk: ivk_ctoption.unwrap(),
            })
        } else {
            Err("generated ivk is the additive identity 0, invalid")
        }
    }
}

impl PartialEq for IncomingViewingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

// TODO: fix IncomingViewingKey
impl SpendingKey {
    /// Generate a new `SpendingKey`.
    ///
    /// When generating, we check that the corresponding `SpendAuthorizingKey`
    /// is not zero, else fail.
    ///
    /// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    pub fn new<T>(csprng: &mut T, network: Network) -> Self
    where
        T: RngCore + CryptoRng,
    {
        loop {
            let mut bytes = [0u8; 32];
            csprng.fill_bytes(&mut bytes);

            let sk = Self::from_bytes(bytes, network);

            // "if ask = 0, discard this key and repeat with a new sk"
            if SpendAuthorizingKey::from(sk).0.is_zero().into() {
                continue;
            }

            // "if ivk ∈ {0, ⊥}, discard this key and repeat with a new sk"
            if IncomingViewingKey::try_from(FullViewingKey::from(sk)).is_err() {
                continue;
            }

            break sk;
        }
    }
}

// TODO: fix IncomingViewingKey
impl From<(IncomingViewingKey, Diversifier)> for TransmissionKey {
    /// This includes _KA^Orchard.DerivePublic(ivk, G_d)_, which is just a
    /// scalar mult _\[ivk\]G_d_.
    ///
    ///  KA^Orchard.DerivePublic(sk, B) := \[sk\] B
    ///
    /// <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>
    /// <https://zips.z.cash/protocol/nu5.pdf#concreteorchardkeyagreement>
    fn from((ivk, d): (IncomingViewingKey, Diversifier)) -> Self {
        let g_d = pallas::Point::from(d);

        Self(pallas::Affine::from(g_d * ivk.ivk))
    }
}

#[allow(clippy::fallible_impl_from)]
impl From<[u8; 32]> for TransmissionKey {
    /// Attempts to interpret a byte representation of an affine point, failing
    /// if the element is not on the curve or non-canonical.
    fn from(bytes: [u8; 32]) -> Self {
        // TODO: turn this unwrap() into a TryFrom error
        Self(pallas::Affine::from_bytes(&bytes).unwrap())
    }
}
