//! Orchard key types.
//!
//! Unused key types are not implemented, see PR #5476.
//!
//! <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>

use std::{fmt, io};

use group::{ff::PrimeField, prime::PrimeCurveAffine, Group, GroupEncoding};
use halo2::{
    arithmetic::{Coordinates, CurveAffine},
    pasta::pallas,
};
use rand_core::{CryptoRng, RngCore};

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

use super::sinsemilla::*;

/// Used to derive a diversified base point from a diversifier value.
///
/// DiversifyHash^Orchard(d) := {︃ GroupHash^P("z.cash:Orchard-gd",""), if P = 0_P
///                               P,                                   otherwise
///
/// where P = GroupHash^P(("z.cash:Orchard-gd", LEBS2OSP_l_d(d)))
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretediversifyhash>
fn diversify_hash(d: &[u8]) -> pallas::Point {
    let p = pallas_group_hash(b"z.cash:Orchard-gd", d);

    if <bool>::from(p.is_identity()) {
        pallas_group_hash(b"z.cash:Orchard-gd", b"")
    } else {
        p
    }
}

/// A _diversifier_, as described in [protocol specification §4.2.3][ps].
///
/// [ps]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Diversifier(pub(crate) [u8; 11]);

impl fmt::Debug for Diversifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Diversifier")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl From<[u8; 11]> for Diversifier {
    fn from(bytes: [u8; 11]) -> Self {
        Self(bytes)
    }
}

impl From<Diversifier> for [u8; 11] {
    fn from(d: Diversifier) -> [u8; 11] {
        d.0
    }
}

impl From<Diversifier> for pallas::Point {
    /// Derive a _diversified base_ point.
    ///
    /// g_d := DiversifyHash^Orchard(d)
    ///
    /// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    fn from(d: Diversifier) -> Self {
        diversify_hash(&d.0)
    }
}

impl PartialEq<[u8; 11]> for Diversifier {
    fn eq(&self, other: &[u8; 11]) -> bool {
        self.0 == *other
    }
}

impl TryFrom<Diversifier> for pallas::Affine {
    type Error = &'static str;

    /// Get a diversified base point from a diversifier value in affine
    /// representation.
    fn try_from(d: Diversifier) -> Result<Self, Self::Error> {
        if let Ok(projective_point) = pallas::Point::try_from(d) {
            Ok(projective_point.into())
        } else {
            Err("Invalid Diversifier -> pallas::Affine")
        }
    }
}

impl Diversifier {
    /// Generate a new `Diversifier`.
    ///
    /// <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>
    pub fn new<T>(csprng: &mut T) -> Self
    where
        T: RngCore + CryptoRng,
    {
        let mut bytes = [0u8; 11];
        csprng.fill_bytes(&mut bytes);

        Self::from(bytes)
    }
}

/// A (diversified) transmission Key
///
/// In Orchard, secrets need to be transmitted to a recipient of funds in order
/// for them to be later spent. To transmit these secrets securely to a
/// recipient without requiring an out-of-band communication channel, the
/// transmission key is used to encrypt them.
///
/// Derived by multiplying a Pallas point [derived][concretediversifyhash] from
/// a `Diversifier` by the `IncomingViewingKey` scalar.
///
/// [concretediversifyhash]: https://zips.z.cash/protocol/nu5.pdf#concretediversifyhash
/// <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>
#[derive(Copy, Clone, PartialEq)]
pub struct TransmissionKey(pub(crate) pallas::Affine);

impl fmt::Debug for TransmissionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_struct("TransmissionKey");

        let option: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();

        match option {
            Some(coordinates) => d
                .field("x", &hex::encode(coordinates.x().to_repr()))
                .field("y", &hex::encode(coordinates.y().to_repr()))
                .finish(),
            None => d
                .field("x", &hex::encode(pallas::Base::zero().to_repr()))
                .field("y", &hex::encode(pallas::Base::zero().to_repr()))
                .finish(),
        }
    }
}

impl Eq for TransmissionKey {}

impl From<TransmissionKey> for [u8; 32] {
    fn from(pk_d: TransmissionKey) -> [u8; 32] {
        pk_d.0.to_bytes()
    }
}

impl PartialEq<[u8; 32]> for TransmissionKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        &self.0.to_bytes() == other
    }
}

/// An ephemeral public key for Orchard key agreement.
///
/// <https://zips.z.cash/protocol/nu5.pdf#concreteorchardkeyagreement>
/// <https://zips.z.cash/protocol/nu5.pdf#saplingandorchardencrypt>
#[derive(Copy, Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct EphemeralPublicKey(#[serde(with = "serde_helpers::Affine")] pub(crate) pallas::Affine);

impl fmt::Debug for EphemeralPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_struct("EphemeralPublicKey");

        let option: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();

        match option {
            Some(coordinates) => d
                .field("x", &hex::encode(coordinates.x().to_repr()))
                .field("y", &hex::encode(coordinates.y().to_repr()))
                .finish(),
            None => d
                .field("x", &hex::encode(pallas::Base::zero().to_repr()))
                .field("y", &hex::encode(pallas::Base::zero().to_repr()))
                .finish(),
        }
    }
}

impl From<EphemeralPublicKey> for [u8; 32] {
    fn from(epk: EphemeralPublicKey) -> [u8; 32] {
        epk.0.to_bytes()
    }
}

impl From<&EphemeralPublicKey> for [u8; 32] {
    fn from(epk: &EphemeralPublicKey) -> [u8; 32] {
        epk.0.to_bytes()
    }
}

impl PartialEq<[u8; 32]> for EphemeralPublicKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        &self.0.to_bytes() == other
    }
}

impl TryFrom<[u8; 32]> for EphemeralPublicKey {
    type Error = &'static str;

    /// Convert an array into a [`EphemeralPublicKey`].
    ///
    /// Returns an error if the encoding is malformed or if [it encodes the
    /// identity point][1].
    ///
    /// > epk cannot be 𝒪_P
    ///
    /// Note that this is [intrinsic to the EphemeralPublicKey][2] type and it is not
    /// a separate consensus rule:
    ///
    /// > Define KA^{Orchard}.Public := P^*.
    ///
    /// [1]: https://zips.z.cash/protocol/protocol.pdf#actiondesc
    /// [2]: https://zips.z.cash/protocol/protocol.pdf#concreteorchardkeyagreement
    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Affine::from_bytes(&bytes);

        if possible_point.is_some().into() {
            let point = possible_point.unwrap();
            if point.to_curve().is_identity().into() {
                Err("pallas::Affine value for Orchard EphemeralPublicKey is the identity")
            } else {
                Ok(Self(possible_point.unwrap()))
            }
        } else {
            Err("Invalid pallas::Affine value for Orchard EphemeralPublicKey")
        }
    }
}

impl ZcashSerialize for EphemeralPublicKey {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 32]>::from(self)[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for EphemeralPublicKey {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Self::try_from(reader.read_32_bytes()?).map_err(SerializationError::Parse)
    }
}
