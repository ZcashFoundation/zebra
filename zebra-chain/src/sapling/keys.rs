//! Sapling key types.
//!
//! Unused key types are not implemented, see PR #5476.
//!
//! "The spend authorizing key ask, proof authorizing key (ak, nsk),
//! full viewing key (ak, nk, ovk), incoming viewing key ivk, and each
//! diversified payment address addr_d = (d, pk_d ) are derived from sk,
//! as described in [Sapling Key Components][ps]." - [§3.1][3.1]
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
//! [3.1]: https://zips.z.cash/protocol/protocol.pdf#addressesandkeys

use std::{fmt, io};

use rand_core::{CryptoRng, RngCore};

use crate::{
    error::{AddressError, RandError},
    primitives::redjubjub::SpendAuth,
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
};

#[cfg(test)]
mod test_vectors;

/// The [Randomness Beacon][1] ("URS").
///
/// First 64 bytes of the BLAKE2s input during JubJub group hash.  URS
/// is a 64-byte US-ASCII string, i.e. the first byte is 0x30, not
/// 0x09.
///
/// From [zcash_primitives][0].
///
/// [0]: https://docs.rs/zcash_primitives/0.2.0/zcash_primitives/constants/constant.GH_FIRST_BLOCK.html
/// [1]: https://zips.z.cash/protocol/protocol.pdf#beacon
pub(super) const RANDOMNESS_BEACON_URS: &[u8; 64] =
    b"096b36a5804bfacef1691e173c366a47ff5ba84a44f26ddd7e8d9f79d5b42df0";

/// GroupHash into Jubjub, aka _GroupHash_URS_
///
/// Produces a random point in the Jubjub curve. The point is
/// guaranteed to be prime order and not the identity. From
/// [zcash_primitives][0].
///
/// d is an 8-byte domain separator ("personalization"), m is the hash
/// input.
///
/// [0]: https://github.com/zcash/librustzcash/blob/master/zcash_primitives/src/group_hash.rs#L15
/// <https://zips.z.cash/protocol/protocol.pdf#concretegrouphashjubjub>
fn jubjub_group_hash(d: [u8; 8], m: &[u8]) -> Option<jubjub::ExtendedPoint> {
    let hash = blake2s_simd::Params::new()
        .hash_length(32)
        .personal(&d)
        .to_state()
        .update(RANDOMNESS_BEACON_URS)
        .update(m)
        .finalize();

    let ct_option = jubjub::AffinePoint::from_bytes(*hash.as_array());

    if ct_option.is_some().unwrap_u8() == 1 {
        let extended_point = ct_option.unwrap().mul_by_cofactor();

        if extended_point != jubjub::ExtendedPoint::identity() {
            Some(extended_point)
        } else {
            None
        }
    } else {
        None
    }
}

/// Used to derive a diversified base point from a diversifier value.
///
/// <https://zips.z.cash/protocol/protocol.pdf#concretediversifyhash>
fn diversify_hash(d: [u8; 11]) -> Option<jubjub::ExtendedPoint> {
    jubjub_group_hash(*b"Zcash_gd", &d)
}

/// A _Diversifier_, as described in [protocol specification §4.2.2][ps].
///
/// Combined with an _IncomingViewingKey_, produces a _diversified
/// payment address_.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
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

impl TryFrom<Diversifier> for jubjub::AffinePoint {
    type Error = &'static str;

    /// Get a diversified base point from a diversifier value in affine
    /// representation.
    fn try_from(d: Diversifier) -> Result<Self, Self::Error> {
        if let Ok(extended_point) = jubjub::ExtendedPoint::try_from(d) {
            Ok(extended_point.into())
        } else {
            Err("Invalid Diversifier -> jubjub::AffinePoint")
        }
    }
}

impl TryFrom<Diversifier> for jubjub::ExtendedPoint {
    type Error = &'static str;

    fn try_from(d: Diversifier) -> Result<Self, Self::Error> {
        let possible_point = diversify_hash(d.0);

        if let Some(point) = possible_point {
            Ok(point)
        } else {
            Err("Invalid Diversifier -> jubjub::ExtendedPoint")
        }
    }
}

impl PartialEq<[u8; 11]> for Diversifier {
    fn eq(&self, other: &[u8; 11]) -> bool {
        self.0 == *other
    }
}

impl Diversifier {
    /// Generate a new _Diversifier_ that has already been confirmed
    /// as a preimage to a valid diversified base point when used to
    /// derive a diversified payment address.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concretediversifyhash>
    pub fn new<T>(csprng: &mut T) -> Result<Self, AddressError>
    where
        T: RngCore + CryptoRng,
    {
        /// Number of times a `diversify_hash` will try to obtain a diversified base point.
        const DIVERSIFY_HASH_TRIES: u8 = 2;

        for _ in 0..DIVERSIFY_HASH_TRIES {
            let mut bytes = [0u8; 11];
            csprng
                .try_fill_bytes(&mut bytes)
                .map_err(|_| AddressError::from(RandError::FillBytes))?;

            if diversify_hash(bytes).is_some() {
                return Ok(Self(bytes));
            }
        }
        Err(AddressError::DiversifierGenerationFailure)
    }
}

/// A (diversified) _TransmissionKey_
///
/// In Sapling, secrets need to be transmitted to a recipient of funds
/// in order for them to be later spent. To transmit these secrets
/// securely to a recipient without requiring an out-of-band
/// communication channel, the diversified transmission key is used to
/// encrypt them.
///
/// Derived by multiplying a JubJub point [derived][ps] from a
/// _Diversifier_ by the _IncomingViewingKey_ scalar.
///
/// The diversified TransmissionKey is denoted `pk_d` in the specification.
/// Note that it can be the identity point, since its type is
/// [`KA^{Sapling}.PublicPrimeSubgroup`][ka] which in turn is [`J^{(r)}`][jubjub].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
/// [ka]: https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement
/// [jubjub]: https://zips.z.cash/protocol/protocol.pdf#jubjub
#[derive(Copy, Clone, PartialEq)]
pub struct TransmissionKey(pub(crate) jubjub::AffinePoint);

impl fmt::Debug for TransmissionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("TransmissionKey")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl Eq for TransmissionKey {}

impl TryFrom<[u8; 32]> for TransmissionKey {
    type Error = &'static str;

    /// Attempts to interpret a byte representation of an affine Jubjub point, failing if the
    /// element is not on the curve, non-canonical, or not in the prime-order subgroup.
    ///
    /// <https://github.com/zkcrypto/jubjub/blob/master/src/lib.rs#L411>
    /// <https://zips.z.cash/zip-0216>
    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let affine_point = jubjub::AffinePoint::from_bytes(bytes).unwrap();
        // Check if it's identity or has prime order (i.e. is in the prime-order subgroup).
        if affine_point.is_torsion_free().into() {
            Ok(Self(affine_point))
        } else {
            Err("Invalid jubjub::AffinePoint value for Sapling TransmissionKey")
        }
    }
}

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

/// An [ephemeral public key][1] for Sapling key agreement.
///
/// Public keys containing points of small order are not allowed.
///
/// It is denoted by `epk` in the specification. (This type does _not_
/// represent [KA^{Sapling}.Public][2], which allows any points, including
/// of small order).
///
/// [1]: https://zips.z.cash/protocol/protocol.pdf#outputdesc
/// [2]: https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement
#[derive(Copy, Clone, Deserialize, PartialEq, Serialize)]
pub struct EphemeralPublicKey(
    #[serde(with = "serde_helpers::AffinePoint")] pub(crate) jubjub::AffinePoint,
);

impl fmt::Debug for EphemeralPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("EphemeralPublicKey")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl Eq for EphemeralPublicKey {}

impl From<EphemeralPublicKey> for [u8; 32] {
    fn from(nk: EphemeralPublicKey) -> [u8; 32] {
        nk.0.to_bytes()
    }
}

impl From<&EphemeralPublicKey> for [u8; 32] {
    fn from(nk: &EphemeralPublicKey) -> [u8; 32] {
        nk.0.to_bytes()
    }
}

impl PartialEq<[u8; 32]> for EphemeralPublicKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        &self.0.to_bytes() == other
    }
}

impl TryFrom<[u8; 32]> for EphemeralPublicKey {
    type Error = &'static str;

    /// Read an EphemeralPublicKey from a byte array.
    ///
    /// Returns an error if the key is non-canonical, or [it is of small order][1].
    ///
    /// # Consensus
    ///
    /// > Check that a Output description's cv and epk are not of small order,
    /// > i.e. \[h_J\]cv MUST NOT be 𝒪_J and \[h_J\]epk MUST NOT be 𝒪_J.
    ///
    /// [1]: https://zips.z.cash/protocol/protocol.pdf#outputdesc
    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = jubjub::AffinePoint::from_bytes(bytes);

        if possible_point.is_none().into() {
            return Err("Invalid jubjub::AffinePoint value for Sapling EphemeralPublicKey");
        }
        if possible_point.unwrap().is_small_order().into() {
            Err("jubjub::AffinePoint value for Sapling EphemeralPublicKey point is of small order")
        } else {
            Ok(Self(possible_point.unwrap()))
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

/// A randomized [validating key][1] that should be used to validate `spendAuthSig`.
///
/// It is denoted by `rk` in the specification. (This type does _not_
/// represent [SpendAuthSig^{Sapling}.Public][2], which allows any points, including
/// of small order).
///
/// [1]: https://zips.z.cash/protocol/protocol.pdf#spenddesc
/// [2]: https://zips.z.cash/protocol/protocol.pdf#concretereddsa
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct ValidatingKey(redjubjub::VerificationKey<SpendAuth>);

impl From<ValidatingKey> for redjubjub::VerificationKey<SpendAuth> {
    fn from(rk: ValidatingKey) -> Self {
        rk.0
    }
}

impl TryFrom<redjubjub::VerificationKey<SpendAuth>> for ValidatingKey {
    type Error = &'static str;

    /// Convert an array into a ValidatingKey.
    ///
    /// Returns an error if the key is malformed or [is of small order][1].
    ///
    /// # Consensus
    ///
    /// > Check that a Spend description's cv and rk are not of small order,
    /// > i.e. \[h_J\]cv MUST NOT be 𝒪_J and \[h_J\]rk MUST NOT be 𝒪_J.
    ///
    /// [1]: https://zips.z.cash/protocol/protocol.pdf#spenddesc
    fn try_from(key: redjubjub::VerificationKey<SpendAuth>) -> Result<Self, Self::Error> {
        if bool::from(
            jubjub::AffinePoint::from_bytes(key.into())
                .unwrap()
                .is_small_order(),
        ) {
            Err("jubjub::AffinePoint value for Sapling ValidatingKey is of small order")
        } else {
            Ok(Self(key))
        }
    }
}

impl TryFrom<[u8; 32]> for ValidatingKey {
    type Error = &'static str;

    fn try_from(value: [u8; 32]) -> Result<Self, Self::Error> {
        let vk = redjubjub::VerificationKey::<SpendAuth>::try_from(value)
            .map_err(|_| "Invalid redjubjub::ValidatingKey for Sapling ValidatingKey")?;
        vk.try_into()
    }
}

impl From<ValidatingKey> for [u8; 32] {
    fn from(key: ValidatingKey) -> Self {
        key.0.into()
    }
}

impl From<ValidatingKey> for redjubjub::VerificationKeyBytes<SpendAuth> {
    fn from(key: ValidatingKey) -> Self {
        key.0.into()
    }
}
