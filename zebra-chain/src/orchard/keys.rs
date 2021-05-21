//! Orchard key types.
//!
//! https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#![allow(clippy::unit_arg)]
#![allow(dead_code)]

#[cfg(test)]
mod tests;

use std::{
    convert::{From, Into, TryFrom, TryInto},
    fmt,
    io::{self, Write},
};

use aes::Aes256;
use bech32::{self, ToBase32, Variant};
use bitvec::prelude::*;
use fpe::ff1::{BinaryNumeralString, FF1};
use group::{Group, GroupEncoding};
use halo2::{
    arithmetic::{Coordinates, CurveAffine, FieldExt},
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
/// https://zips.z.cash/protocol/nu5.pdf#concreteprps
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

/// Invokes Blake2b-512 as PRF^expand with parameter t.
///
/// PRF^expand(sk, t) := BLAKE2b-512("Zcash_ExpandSeed", sk || t)
///
/// https://zips.z.cash/protocol/nu5.pdf#concreteprfs
// TODO: This is basically a duplicate of the one in our sapling module, its
// definition in the draft Nu5 spec is incomplete so I'm putting it here in case
// it changes.
pub fn prf_expand(sk: [u8; 32], t: Vec<&[u8]>) -> [u8; 64] {
    let mut state = blake2b_simd::Params::new()
        .hash_length(64)
        .personal(b"Zcash_ExpandSeed")
        .to_state();

    state.update(&sk[..]);

    for t_i in t {
        state.update(t_i);
    }

    *state.finalize().as_array()
}

/// Used to derive the outgoing cipher key _ock_ used to encrypt an encrypted
/// output note from an Action.
///
/// PRF^ock(ovk, cv, cm_x, ephemeralKey) := BLAKE2b-256(“Zcash_Orchardock”, ovk || cv || cm_x || ephemeralKey)
///
/// https://zips.z.cash/protocol/nu5.pdf#concreteprfs
/// https://zips.z.cash/protocol/nu5.pdf#concretesym
fn prf_ock(ovk: [u8; 32], cv: [u8; 32], cm_x: [u8; 32], ephemeral_key: [u8; 32]) -> [u8; 32] {
    let hash = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"Zcash_Orchardock")
        .to_state()
        .update(&ovk)
        .update(&cv)
        .update(&cm_x)
        .update(&ephemeral_key)
        .finalize();

    hash.as_bytes().try_into().expect("32 byte array")
}

/// Used to derive a diversified base point from a diversifier value.
///
/// DiversifyHash^Orchard(d) := {︃ GroupHash^P("z.cash:Orchard-gd",""), if P = 0_P
///                               P,                                   otherwise
///
/// where P = GroupHash^P(("z.cash:Orchard-gd", LEBS2OSP_l_d(d)))
///
/// https://zips.z.cash/protocol/nu5.pdf#concretediversifyhash
fn diversify_hash(d: &[u8]) -> pallas::Point {
    let p = pallas_group_hash(b"z.cash:Orchard-gd", &d);

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
    bytes: [u8; 32],
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
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => sk_hrp::MAINNET,
            Network::Testnet => sk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.bytes.to_base32(), Variant::Bech32).unwrap()
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
            if SpendAuthorizingKey::from(sk).0 == pallas::Scalar::zero() {
                continue;
            }

            break sk;
        }
    }

    /// Generate a `SpendingKey` from existing bytes.
    fn from_bytes(bytes: [u8; 32], network: Network) -> Self {
        Self { network, bytes }
    }
}

/// A Spend authorizing key (_ask_), as described in [protocol specification
/// §4.2.3][orchardkeycomponents].
///
/// Used to generate _spend authorization randomizers_ to sign each _Action
/// Description_ that spends notes, proving ownership of notes.
///
/// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone)]
pub struct SpendAuthorizingKey(pub(crate) pallas::Scalar);

impl ConstantTimeEq for SpendAuthorizingKey {
    /// Check whether two `SpendAuthorizingKey`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.to_bytes().ct_eq(&other.0.to_bytes())
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
        sk.0.to_bytes()
    }
}

impl From<SpendingKey> for SpendAuthorizingKey {
    /// Invokes Blake2b-512 as _PRF^expand_, t=6, to derive a
    /// `SpendAuthorizingKey` from a `SpendingKey`.
    ///
    /// ask := ToScalar^Orchard(PRF^expand(sk, [6]))
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    /// https://zips.z.cash/protocol/nu5.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> SpendAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.bytes, vec![&[6]]);

        // Handles ToScalar^Orchard
        Self(pallas::Scalar::from_bytes_wide(&hash_bytes))
    }
}

impl Eq for SpendAuthorizingKey {}

impl PartialEq for SpendAuthorizingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for SpendAuthorizingKey {
    // TODO: do we want to use constant-time comparison here?
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_bytes().ct_eq(other).unwrap_u8() == 1u8
    }
}

/// A Spend validating key, as described in [protocol specification
/// §4.2.3][orchardkeycomponents].
///
/// Used to validate Orchard _Spend Authorization Signatures_, proving ownership
/// of notes.
///
/// [orchardkeycomponents]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone, Debug)]
pub struct SpendValidatingKey(pub(crate) redpallas::VerificationKey<SpendAuth>);

impl Eq for SpendValidatingKey {}

impl From<[u8; 32]> for SpendValidatingKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(redpallas::VerificationKey::try_from(bytes).unwrap())
    }
}

impl From<SpendValidatingKey> for [u8; 32] {
    fn from(ak: SpendValidatingKey) -> [u8; 32] {
        ak.0.into()
    }
}

impl From<SpendAuthorizingKey> for SpendValidatingKey {
    fn from(ask: SpendAuthorizingKey) -> Self {
        let sk = redpallas::SigningKey::<SpendAuth>::try_from(<[u8; 32]>::from(ask)).unwrap();

        Self(redpallas::VerificationKey::from(&sk))
    }
}

impl PartialEq for SpendValidatingKey {
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &Self) -> bool {
        // XXX: These redpallas::VerificationKey(Bytes) fields are pub(crate)
        self.0.bytes.bytes == other.0.bytes.bytes
    }
}

impl PartialEq<[u8; 32]> for SpendValidatingKey {
    #[allow(clippy::cmp_owned)]
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
        self.0.to_bytes().ct_eq(&other.0.to_bytes())
    }
}

impl fmt::Debug for NullifierDerivingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("NullifierDerivingKey")
            .field(&hex::encode(self.0.to_bytes()))
            .finish()
    }
}

impl Eq for NullifierDerivingKey {}

impl From<NullifierDerivingKey> for [u8; 32] {
    fn from(nk: NullifierDerivingKey) -> [u8; 32] {
        nk.0.to_bytes()
    }
}

impl From<&NullifierDerivingKey> for [u8; 32] {
    fn from(nk: &NullifierDerivingKey) -> [u8; 32] {
        nk.0.to_bytes()
    }
}

impl From<NullifierDerivingKey> for pallas::Base {
    fn from(nk: NullifierDerivingKey) -> pallas::Base {
        nk.0
    }
}

impl From<[u8; 32]> for NullifierDerivingKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(pallas::Base::from_bytes(&bytes).unwrap())
    }
}

impl From<SpendingKey> for NullifierDerivingKey {
    /// nk = ToBase^Orchard(PRF^expand_sk ([7]))
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    fn from(sk: SpendingKey) -> Self {
        Self(pallas::Base::from_bytes_wide(&prf_expand(
            sk.into(),
            vec![&[7]],
        )))
    }
}

impl PartialEq for NullifierDerivingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for NullifierDerivingKey {
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_bytes().ct_eq(other).unwrap_u8() == 1u8
    }
}

/// Commit^ivk randomness.
///
/// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
// XXX: Should this be replaced by commitment::CommitmentRandomness?
#[derive(Copy, Clone)]
pub struct IvkCommitRandomness(pub(crate) pallas::Scalar);

impl ConstantTimeEq for IvkCommitRandomness {
    /// Check whether two `IvkCommitRandomness`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.to_bytes().ct_eq(&other.0.to_bytes())
    }
}

impl fmt::Debug for IvkCommitRandomness {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("IvkCommitRandomness")
            .field(&hex::encode(self.0.to_bytes()))
            .finish()
    }
}

impl Eq for IvkCommitRandomness {}

impl From<SpendingKey> for IvkCommitRandomness {
    /// rivk = ToScalar^Orchard(PRF^expand_sk ([8]))
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    fn from(sk: SpendingKey) -> Self {
        let scalar = pallas::Scalar::from_bytes_wide(&prf_expand(sk.into(), vec![&[8]]));

        Self(scalar)
    }
}

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
        self.0.to_bytes().ct_eq(other).unwrap_u8() == 1u8
    }
}

impl TryFrom<[u8; 32]> for IvkCommitRandomness {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_scalar = pallas::Scalar::from_bytes(&bytes);

        if possible_scalar.is_some().into() {
            Ok(Self(possible_scalar.unwrap()))
        } else {
            Err("Invalid pallas::Scalar value")
        }
    }
}

/// Magic human-readable strings used to identify what networks Orchard incoming
/// viewing keys are associated with when encoded/decoded with bech32.
///
/// https://zips.z.cash/protocol/nu5.pdf#orchardinviewingkeyencoding
mod ivk_hrp {
    pub const MAINNET: &str = "zivko";
    pub const TESTNET: &str = "zivktestorchard";
}

/// An incoming viewing key, as described in [protocol specification
/// §4.2.3][ps].
///
/// Used to decrypt incoming notes without spending them.
///
/// [ps]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone)]
pub struct IncomingViewingKey {
    network: Network,
    scalar: pallas::Scalar,
}

impl ConstantTimeEq for IncomingViewingKey {
    /// Check whether two `IncomingViewingKey`s are equal, runtime independent
    /// of the value of the secret.
    ///
    /// # Note
    ///
    /// This function short-circuits if the networks of the keys are different.
    /// Otherwise, it should execute in time independent of the `bytes` value.
    fn ct_eq(&self, other: &Self) -> Choice {
        if self.network != other.network {
            return Choice::from(0);
        }

        self.scalar.to_bytes().ct_eq(&other.scalar.to_bytes())
    }
}

impl fmt::Debug for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("IncomingViewingKey")
            .field(&hex::encode(self.scalar.to_bytes()))
            .finish()
    }
}

impl fmt::Display for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => ivk_hrp::MAINNET,
            Network::Testnet => ivk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.scalar.to_bytes().to_base32(), Variant::Bech32).unwrap()
    }
}

impl Eq for IncomingViewingKey {}

impl From<FullViewingKey> for IncomingViewingKey {
    /// Commit^ivk_rivk(ak, nk) :=
    ///    SinsemillaShortCommit_rcm (︁"z.cash:Orchard-CommitIvk", I2LEBSP_l(ak) || I2LEBSP_l(nk)︁) mod r_P
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    /// https://zips.z.cash/protocol/nu5.pdf#concreteprfs
    #[allow(non_snake_case)]
    fn from(fvk: FullViewingKey) -> Self {
        let mut M: BitVec<Lsb0, u8> = BitVec::new();

        M.append(&mut BitVec::<Lsb0, u8>::from_slice(
            &<[u8; 32]>::from(fvk.spend_validating_key)[..],
        ));
        M.append(&mut BitVec::<Lsb0, u8>::from_slice(
            &<[u8; 32]>::from(fvk.nullifier_deriving_key)[..],
        ));

        // Commit^ivk_rivk
        let commit_x = sinsemilla_short_commit(
            fvk.ivk_commit_randomness.into(),
            b"z.cash:Orchard-CommitIvk",
            &M,
        );

        Self {
            network: fvk.network,
            // mod r_P
            scalar: pallas::Scalar::from_bytes(&commit_x.into()).unwrap(),
        }
    }
}

impl PartialEq for IncomingViewingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for IncomingViewingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.scalar.to_bytes().ct_eq(other).unwrap_u8() == 1u8
    }
}

impl IncomingViewingKey {
    /// Generate an _IncomingViewingKey_ from existing bytes and a network variant.
    fn from_bytes(bytes: [u8; 32], network: Network) -> Self {
        Self {
            network,
            scalar: pallas::Scalar::from_bytes(&bytes).unwrap(),
        }
    }
}

/// Magic human-readable strings used to identify what networks Orchard full
/// viewing keys are associated with when encoded/decoded with bech32.
///
/// https://zips.z.cash/protocol/nu5.pdf#orchardfullviewingkeyencoding
mod fvk_hrp {
    pub const MAINNET: &str = "zviewo";
    pub const TESTNET: &str = "zviewtestorchard";
}

/// Full Viewing Keys
///
/// Allows recognizing both incoming and outgoing notes without having
/// spend authority.
///
/// For incoming viewing keys on the production network, the
/// Human-Readable Part is “zviewo”. For incoming viewing keys on the
/// test network, the Human-Readable Part is “zviewtestorchard”.
///
/// https://zips.z.cash/protocol/nu5.pdf#orchardfullviewingkeyencoding
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct FullViewingKey {
    network: Network,
    spend_validating_key: SpendValidatingKey,
    nullifier_deriving_key: NullifierDerivingKey,
    ivk_commit_randomness: IvkCommitRandomness,
}

impl FullViewingKey {
    /// [4.2.3]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    #[allow(non_snake_case)]
    pub fn to_R(self) -> [u8; 64] {
        // let K = I2LEBSP_l_sk(rivk)
        let K: [u8; 32] = self.ivk_commit_randomness.into();

        let mut t: Vec<&[u8]> = vec![&[0x82u8]];

        let ak_bytes = <[u8; 32]>::from(self.spend_validating_key);
        t.push(&ak_bytes);

        let nk_bytes = <[u8; 32]>::from(self.nullifier_deriving_key);
        t.push(&nk_bytes);

        // let R = PRF^expand_K( [0x82] || I2LEOSP256(ak) || I2LEOSP256(nk) )
        prf_expand(K, t)
    }

    /// Derive a full viewing key from a existing spending key and its network.
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#addressesandkeys
    /// https://zips.z.cash/protocol/nu5.pdf#orchardfullviewingkeyencoding
    pub fn from_spending_key(sk: SpendingKey) -> FullViewingKey {
        let spend_authorizing_key = SpendAuthorizingKey::from(sk);

        Self {
            network: sk.network,
            spend_validating_key: SpendValidatingKey::from(spend_authorizing_key),
            nullifier_deriving_key: NullifierDerivingKey::from(sk),
            ivk_commit_randomness: IvkCommitRandomness::from(sk),
        }
    }
}

impl fmt::Debug for FullViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FullViewingKey")
            .field("network", &self.network)
            .field("spend_validating_key", &self.spend_validating_key)
            .field("nullifier_deriving_key", &self.nullifier_deriving_key)
            .field("ivk_commit_randomness", &self.ivk_commit_randomness)
            .finish()
    }
}

impl fmt::Display for FullViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = bytes.write_all(&<[u8; 32]>::from(self.spend_validating_key));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.nullifier_deriving_key));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.ivk_commit_randomness));

        let hrp = match self.network {
            Network::Mainnet => fvk_hrp::MAINNET,
            Network::Testnet => fvk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, bytes.get_ref().to_base32(), Variant::Bech32).unwrap()
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
            .field(&hex::encode(&self.0))
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

impl From<FullViewingKey> for OutgoingViewingKey {
    /// Derive an `OutgoingViewingKey` from a `FullViewingKey`.
    ///
    /// [4.2.3]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    #[allow(non_snake_case)]
    fn from(fvk: FullViewingKey) -> OutgoingViewingKey {
        let R = fvk.to_R();

        // let ovk be the remaining [32] bytes of R [which is 64 bytes]
        let ovk_bytes: [u8; 32] = R[32..64].try_into().expect("32 byte array");

        Self::from(ovk_bytes)
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
#[derive(Copy, Clone, PartialEq)]
pub struct DiversifierKey([u8; 32]);

impl From<FullViewingKey> for DiversifierKey {
    /// Derives a _diversifier key_ from a `FullViewingKey`.
    ///
    /// 'For each spending key, there is also a default diversified
    /// payment address with a “random-looking” diversifier. This
    /// allows an implementation that does not expose diversified
    /// addresses as a user-visible feature, to use a default address
    /// that cannot be distinguished (without knowledge of the
    /// spending key) from one with a random diversifier...'
    ///
    /// Derived as specied in section [4.2.3] of the spec, and [ZIP-32].
    ///
    /// [4.2.3]: https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    /// [ZIP-32]: https://zips.z.cash/zip-0032#orchard-diversifier-derivation
    #[allow(non_snake_case)]
    fn from(fvk: FullViewingKey) -> DiversifierKey {
        let R = fvk.to_R();

        // "let dk be the first [32] bytes of R"
        Self(R[..32].try_into().expect("subslice of R is a valid array"))
    }
}

impl From<DiversifierKey> for [u8; 32] {
    fn from(dk: DiversifierKey) -> [u8; 32] {
        dk.0
    }
}

/// A _diversifier_, as described in [protocol specification §4.2.3][ps].
///
/// Combined with an `IncomingViewingKey`, produces a _diversified
/// payment address_.
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
            .field(&hex::encode(&self.0))
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
    /// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
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
/// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
#[derive(Copy, Clone, PartialEq)]
pub struct TransmissionKey(pub(crate) pallas::Affine);

impl fmt::Debug for TransmissionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_struct("TransmissionKey");

        let option: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();

        match option {
            Some(coordinates) => d
                .field("x", &hex::encode(coordinates.x().to_bytes()))
                .field("y", &hex::encode(coordinates.y().to_bytes()))
                .finish(),
            None => d
                .field("x", &hex::encode(pallas::Base::zero().to_bytes()))
                .field("y", &hex::encode(pallas::Base::zero().to_bytes()))
                .finish(),
        }
    }
}

impl Eq for TransmissionKey {}

impl From<[u8; 32]> for TransmissionKey {
    /// Attempts to interpret a byte representation of an affine point, failing
    /// if the element is not on the curve or non-canonical.
    ///
    /// https://github.com/zkcrypto/jubjub/blob/master/src/lib.rs#L411
    fn from(bytes: [u8; 32]) -> Self {
        Self(pallas::Affine::from_bytes(&bytes).unwrap())
    }
}

impl From<TransmissionKey> for [u8; 32] {
    fn from(pk_d: TransmissionKey) -> [u8; 32] {
        pk_d.0.to_bytes()
    }
}

impl From<(IncomingViewingKey, Diversifier)> for TransmissionKey {
    /// This includes _KA^Orchard.DerivePublic(ivk, G_d)_, which is just a
    /// scalar mult _\[ivk\]G_d_.
    ///
    ///  KA^Orchard.DerivePublic(sk, B) := [sk] B
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#orchardkeycomponents
    /// https://zips.z.cash/protocol/nu5.pdf#concreteorchardkeyagreement
    fn from((ivk, d): (IncomingViewingKey, Diversifier)) -> Self {
        let g_d = pallas::Point::from(d);

        Self(pallas::Affine::from(g_d * ivk.scalar))
    }
}

impl PartialEq<[u8; 32]> for TransmissionKey {
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        &self.0.to_bytes() == other
    }
}

/// An ephemeral public key for Orchard key agreement.
///
/// https://zips.z.cash/protocol/nu5.pdf#concreteorchardkeyagreement
/// https://zips.z.cash/protocol/nu5.pdf#saplingandorchardencrypt
#[derive(Copy, Clone, Deserialize, PartialEq, Serialize)]
pub struct EphemeralPublicKey(#[serde(with = "serde_helpers::Affine")] pub(crate) pallas::Affine);

impl fmt::Debug for EphemeralPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_struct("EphemeralPublicKey");

        let option: Option<Coordinates<pallas::Affine>> = self.0.coordinates().into();

        match option {
            Some(coordinates) => d
                .field("x", &hex::encode(coordinates.x().to_bytes()))
                .field("y", &hex::encode(coordinates.y().to_bytes()))
                .finish(),
            None => d
                .field("x", &hex::encode(pallas::Base::zero().to_bytes()))
                .field("y", &hex::encode(pallas::Base::zero().to_bytes()))
                .finish(),
        }
    }
}

impl Eq for EphemeralPublicKey {}

impl From<&EphemeralPublicKey> for [u8; 32] {
    fn from(epk: &EphemeralPublicKey) -> [u8; 32] {
        epk.0.to_bytes()
    }
}

impl PartialEq<[u8; 32]> for EphemeralPublicKey {
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        &self.0.to_bytes() == other
    }
}

impl TryFrom<[u8; 32]> for EphemeralPublicKey {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Affine::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err("Invalid pallas::Affine value")
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
        Self::try_from(reader.read_32_bytes()?).map_err(|e| SerializationError::Parse(e))
    }
}

/// An _outgoing cipher key_ for Orchard note encryption/decryption.
///
/// https://zips.z.cash/protocol/nu5.pdf#saplingandorchardencrypt
#[derive(Copy, Clone, PartialEq)]
pub struct OutgoingCipherKey([u8; 32]);

impl fmt::Debug for OutgoingCipherKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("OutgoingCipherKey")
            .field(&hex::encode(self.0))
            .finish()
    }
}

impl From<&OutgoingCipherKey> for [u8; 32] {
    fn from(ock: &OutgoingCipherKey) -> [u8; 32] {
        ock.0
    }
}

// TODO: derive `OutgoingCipherKey`: https://github.com/ZcashFoundation/zebra/issues/2041
