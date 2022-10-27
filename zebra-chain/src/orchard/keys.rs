//! Orchard key types.
//!
//! Some unused key types are not implemented.
//!
//! <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>

use std::{fmt, io};

use aes::Aes256;
use bech32::{self, ToBase32, Variant};
use fpe::ff1::{BinaryNumeralString, FF1};
use group::{ff::PrimeField, prime::PrimeCurveAffine, Group, GroupEncoding};
use halo2::{
    arithmetic::{Coordinates, CurveAffine},
    pasta::pallas,
};
use rand_core::{CryptoRng, RngCore};
use subtle::{Choice, ConstantTimeEq};

use crate::{
    parameters::Network,
    primitives::redpallas::{self, SpendAuth},
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
};

use super::sinsemilla::*;

/// PRP^d_K(d) := FF1-AES256_K("", d)
///
/// "Let FF1-AES256_K(tweak, x) be the FF1 format-preserving encryption
/// algorithm using AES with a 256-bit key K, and parameters radix = 2, minlen =
/// 88, maxlen = 88. It will be used only with the empty string "" as the
/// tweak. x is a sequence of 88 bits, as is the output."
///
/// <https://zips.z.cash/protocol/nu5.pdf#concreteprps>
#[allow(non_snake_case)]
fn prp_d(K: [u8; 32], d: [u8; 11]) -> [u8; 11] {
    let radix = 2;
    let tweak = b"";

    let ff = FF1::<Aes256>::new(&K, radix).expect("valid radix");

    ff.encrypt(tweak, &BinaryNumeralString::from_bytes_le(&d))
        .unwrap()
        .to_bytes_le()
        .try_into()
        .unwrap()
}

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

/// Magic human-readable strings used to identify what networks Orchard spending
/// keys are associated with when encoded/decoded with bech32.
///
/// [orchardspendingkeyencoding]: https://zips.z.cash/protocol/nu5.pdf#orchardspendingkeyencoding
mod sk_hrp {
    pub const MAINNET: &str = "secret-orchard-sk-main";
    pub const TESTNET: &str = "secret-orchard-sk-test";
}

/// A spending key, as described in [protocol specification §4.2.3][ps].
///
/// Our root secret key of the Orchard key derivation tree. All other Orchard
/// key types derive from the [`SpendingKey`] value.
///
/// [ps]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone, Debug)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct SpendingKey {
    network: Network,
    pub(super) bytes: [u8; 32],
}

impl ConstantTimeEq for SpendingKey {
    /// Check whether two `SpendingKey`s are equal, runtime independent of the
    /// value of the secret.
    ///
    /// # Note
    ///
    /// This function short-circuits if the networks of the keys are different.
    /// Otherwise, it should execute in time independent of the `bytes` value.
    fn ct_eq(&self, other: &Self) -> Choice {
        if self.network != other.network {
            return Choice::from(0);
        }

        self.bytes.ct_eq(&other.bytes)
    }
}

impl fmt::Display for SpendingKey {
    #[allow(clippy::unwrap_in_result)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => sk_hrp::MAINNET,
            Network::Testnet => sk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.bytes.to_base32(), Variant::Bech32)
            .expect("hrp is valid")
    }
}

impl From<SpendingKey> for [u8; 32] {
    fn from(sk: SpendingKey) -> Self {
        sk.bytes
    }
}

impl Eq for SpendingKey {}

impl PartialEq for SpendingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl SpendingKey {
    /// Generate a `SpendingKey` from existing bytes.
    pub fn from_bytes(bytes: [u8; 32], network: Network) -> Self {
        Self { network, bytes }
    }

    /// Returns the network for this spending key.
    pub fn network(&self) -> Network {
        self.network
    }
}

/// A Spend authorizing key (_ask_), as described in [protocol specification
/// §4.2.3][orchardkeycomponents].
///
/// Used to generate _spend authorization randomizers_ to sign each _Action
/// Description_ that spends notes, proving ownership of notes.
///
/// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone, Eq)]
pub struct SpendAuthorizingKey(pub(crate) pallas::Scalar);

impl ConstantTimeEq for SpendAuthorizingKey {
    /// Check whether two `SpendAuthorizingKey`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.to_repr().ct_eq(&other.0.to_repr())
    }
}

impl fmt::Debug for SpendAuthorizingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("SpendAuthorizingKey")
            .field(&hex::encode(<[u8; 32]>::from(*self)))
            .finish()
    }
}

impl From<SpendAuthorizingKey> for [u8; 32] {
    fn from(sk: SpendAuthorizingKey) -> Self {
        sk.0.to_repr()
    }
}

impl PartialEq for SpendAuthorizingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for SpendAuthorizingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_repr().ct_eq(other).unwrap_u8() == 1u8
    }
}

/// A Spend validating key, as described in [protocol specification
/// §4.2.3][orchardkeycomponents].
///
/// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone, Debug)]
pub struct SpendValidatingKey(pub(crate) redpallas::VerificationKey<SpendAuth>);

impl Eq for SpendValidatingKey {}

impl From<SpendValidatingKey> for [u8; 32] {
    fn from(ak: SpendValidatingKey) -> [u8; 32] {
        ak.0.into()
    }
}

impl From<SpendAuthorizingKey> for SpendValidatingKey {
    fn from(ask: SpendAuthorizingKey) -> Self {
        let sk = redpallas::SigningKey::<SpendAuth>::try_from(<[u8; 32]>::from(ask)).expect(
            "a scalar converted to byte array and then converted back to a scalar should not fail",
        );

        Self(redpallas::VerificationKey::from(&sk))
    }
}

impl PartialEq for SpendValidatingKey {
    fn eq(&self, other: &Self) -> bool {
        // XXX: These redpallas::VerificationKey(Bytes) fields are pub(crate)
        self.0.bytes.bytes == other.0.bytes.bytes
    }
}

impl PartialEq<[u8; 32]> for SpendValidatingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        // XXX: These redpallas::VerificationKey(Bytes) fields are pub(crate)
        self.0.bytes.bytes == *other
    }
}

/// A Orchard nullifier deriving key, as described in [protocol specification
/// §4.2.3][orchardkeycomponents].
///
/// Used to create a _Nullifier_ per note.
///
/// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone)]
pub struct NullifierDerivingKey(pub(crate) pallas::Base);

impl ConstantTimeEq for NullifierDerivingKey {
    /// Check whether two `NullifierDerivingKey`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.to_repr().ct_eq(&other.0.to_repr())
    }
}

impl fmt::Debug for NullifierDerivingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("NullifierDerivingKey")
            .field(&hex::encode(self.0.to_repr()))
            .finish()
    }
}

impl Eq for NullifierDerivingKey {}

impl From<NullifierDerivingKey> for [u8; 32] {
    fn from(nk: NullifierDerivingKey) -> [u8; 32] {
        nk.0.to_repr()
    }
}

impl From<&NullifierDerivingKey> for [u8; 32] {
    fn from(nk: &NullifierDerivingKey) -> [u8; 32] {
        nk.0.to_repr()
    }
}

impl From<NullifierDerivingKey> for pallas::Base {
    fn from(nk: NullifierDerivingKey) -> pallas::Base {
        nk.0
    }
}

impl From<[u8; 32]> for NullifierDerivingKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(pallas::Base::from_repr(bytes).unwrap())
    }
}

impl PartialEq for NullifierDerivingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for NullifierDerivingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_repr().ct_eq(other).unwrap_u8() == 1u8
    }
}

/// Commit^ivk randomness.
///
/// <https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents>
// XXX: Should this be replaced by commitment::CommitmentRandomness?
#[derive(Copy, Clone)]
pub struct IvkCommitRandomness(pub(crate) pallas::Scalar);

impl ConstantTimeEq for IvkCommitRandomness {
    /// Check whether two `IvkCommitRandomness`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.to_repr().ct_eq(&other.0.to_repr())
    }
}

impl fmt::Debug for IvkCommitRandomness {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("IvkCommitRandomness")
            .field(&hex::encode(self.0.to_repr()))
            .finish()
    }
}

impl Eq for IvkCommitRandomness {}

impl From<IvkCommitRandomness> for [u8; 32] {
    fn from(rivk: IvkCommitRandomness) -> Self {
        rivk.0.into()
    }
}

impl From<IvkCommitRandomness> for pallas::Scalar {
    fn from(rivk: IvkCommitRandomness) -> Self {
        rivk.0
    }
}

impl PartialEq for IvkCommitRandomness {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for IvkCommitRandomness {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_repr().ct_eq(other).unwrap_u8() == 1u8
    }
}

impl TryFrom<[u8; 32]> for IvkCommitRandomness {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_scalar = pallas::Scalar::from_repr(bytes);

        if possible_scalar.is_some().into() {
            Ok(Self(possible_scalar.unwrap()))
        } else {
            Err("Invalid pallas::Scalar value")
        }
    }
}

/// _Full viewing keys_
///
/// Allows recognizing both incoming and outgoing notes without having
/// spend authority.
///
/// <https://zips.z.cash/protocol/nu5.pdf#orchardfullviewingkeyencoding>
#[derive(Copy, Clone)]
pub struct FullViewingKey {
    pub(super) spend_validating_key: SpendValidatingKey,
    pub(super) nullifier_deriving_key: NullifierDerivingKey,
    pub(super) ivk_commit_randomness: IvkCommitRandomness,
}

impl ConstantTimeEq for FullViewingKey {
    /// Check whether two `FullViewingKey`s are equal, runtime independent of
    /// the value of the secrets.
    ///
    /// # Note
    ///
    /// This function short-circuits if the spend validating keys
    /// are different.  Otherwise, it should execute in time independent of the
    /// secret component values.
    fn ct_eq(&self, other: &Self) -> Choice {
        if self.spend_validating_key != other.spend_validating_key {
            return Choice::from(0);
        }

        // Uses std::ops::BitAnd
        self.nullifier_deriving_key
            .ct_eq(&other.nullifier_deriving_key)
            & self
                .ivk_commit_randomness
                .ct_eq(&other.ivk_commit_randomness)
    }
}

impl fmt::Debug for FullViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FullViewingKey")
            .field("spend_validating_key", &self.spend_validating_key)
            .field("nullifier_deriving_key", &self.nullifier_deriving_key)
            .field("ivk_commit_randomness", &self.ivk_commit_randomness)
            .finish()
    }
}

impl fmt::Display for FullViewingKey {
    /// The _raw encoding_ of an **Orchard** _full viewing key_.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#orchardfullviewingkeyencoding>
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&hex::encode(<[u8; 96]>::from(*self)))
    }
}

impl From<FullViewingKey> for [u8; 96] {
    fn from(fvk: FullViewingKey) -> [u8; 96] {
        let mut bytes = [0u8; 96];

        bytes[..32].copy_from_slice(&<[u8; 32]>::from(fvk.spend_validating_key));
        bytes[32..64].copy_from_slice(&<[u8; 32]>::from(fvk.nullifier_deriving_key));
        bytes[64..].copy_from_slice(&<[u8; 32]>::from(fvk.ivk_commit_randomness));

        bytes
    }
}

impl PartialEq for FullViewingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

/// An outgoing viewing key, as described in [protocol specification
/// §4.2.3][ps].
///
/// Used to decrypt outgoing notes without spending them.
///
/// [ps]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone)]
pub struct OutgoingViewingKey(pub(crate) [u8; 32]);

impl ConstantTimeEq for OutgoingViewingKey {
    /// Check whether two `OutgoingViewingKey`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.ct_eq(&other.0)
    }
}

impl fmt::Debug for OutgoingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("OutgoingViewingKey")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl Eq for OutgoingViewingKey {}

impl From<[u8; 32]> for OutgoingViewingKey {
    /// Generate an `OutgoingViewingKey` from existing bytes.
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<OutgoingViewingKey> for [u8; 32] {
    fn from(ovk: OutgoingViewingKey) -> [u8; 32] {
        ovk.0
    }
}

impl PartialEq for OutgoingViewingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for OutgoingViewingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.ct_eq(other).unwrap_u8() == 1u8
    }
}

/// A _diversifier key_.
///
/// "We define a mechanism for deterministically deriving a sequence of
/// diversifiers, without leaking how many diversified addresses have already
/// been generated for an account. Unlike Sapling, we do so by deriving a
/// _diversifier key_ directly from the _full viewing key_, instead of as part
/// of the _extended spending key_. This means that the _full viewing key_
/// provides the capability to determine the position of a _diversifier_ within
/// the sequence, which matches the capabilities of a Sapling _extended full
/// viewing key_ but simplifies the key structure."
///
/// [4.2.3]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
/// [ZIP-32]: https://zips.z.cash/zip-0032#orchard-diversifier-derivation
#[derive(Copy, Clone, Debug)]
pub struct DiversifierKey([u8; 32]);

impl ConstantTimeEq for DiversifierKey {
    /// Check whether two `DiversifierKey`s are equal, runtime independent of
    /// the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.ct_eq(&other.0)
    }
}

impl Eq for DiversifierKey {}

impl From<[u8; 32]> for DiversifierKey {
    fn from(bytes: [u8; 32]) -> DiversifierKey {
        DiversifierKey(bytes)
    }
}

impl From<DiversifierKey> for [u8; 32] {
    fn from(dk: DiversifierKey) -> [u8; 32] {
        dk.0
    }
}

impl PartialEq for DiversifierKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for DiversifierKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.ct_eq(other).unwrap_u8() == 1u8
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

impl From<DiversifierKey> for Diversifier {
    /// Generates the _default diversifier_, where the index into the
    /// `DiversifierKey` is 0.
    ///
    /// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    fn from(dk: DiversifierKey) -> Self {
        Self(prp_d(dk.into(), [0u8; 11]))
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

// TODO:
// - implement and derive OutgoingCipherKey: #2041, #5476
// - implement EphemeralPrivateKey: #2192, #5476

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
