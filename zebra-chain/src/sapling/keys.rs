//! Key types.
//!
//! "The spend authorizing key ask, proof authorizing key (ak, nsk),
//! full viewing key (ak, nk, ovk), incoming viewing key ivk, and each
//! diversified payment address addr_d = (d, pk_d ) are derived from sk,
//! as described in [Sapling Key Components][ps]." - [§3.1][3.1]
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
//! [3.1]: https://zips.z.cash/protocol/protocol.pdf#addressesandkeys
#![allow(clippy::unit_arg)]
#![allow(clippy::fallible_impl_from)]
#![allow(dead_code)]

#[cfg(test)]
mod test_vectors;
#[cfg(test)]
mod tests;

use std::{
    convert::{From, Into, TryFrom, TryInto},
    fmt,
    io::{self, Write},
    str::FromStr,
};

use bech32::{self, FromBase32, ToBase32, Variant};
use rand_core::{CryptoRng, RngCore};
use subtle::{Choice, ConstantTimeEq};

use crate::{
    parameters::Network,
    primitives::redjubjub::{self, SpendAuth},
    serialization::{
        serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
    },
};

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

/// Invokes Blake2b-512 as PRF^expand with parameter t, to derive a
/// SpendAuthorizingKey and ProofAuthorizingKey from SpendingKey.
///
/// PRF^expand(sk, t) := BLAKE2b-512("Zcash_ExpandSeed", sk || t)
///
/// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
fn prf_expand(sk: [u8; 32], t: &[u8]) -> [u8; 64] {
    let hash = blake2b_simd::Params::new()
        .hash_length(64)
        .personal(b"Zcash_ExpandSeed")
        .to_state()
        .update(&sk[..])
        .update(t)
        .finalize();

    *hash.as_array()
}

/// Used to derive the outgoing cipher key _ock_ used to encrypt an Output ciphertext.
///
/// PRF^ock(ovk, cv, cm_u, ephemeralKey) := BLAKE2b-256(“Zcash_Derive_ock”, ovk || cv || cm_u || ephemeralKey)
///
/// <https://zips.z.cash/protocol/nu5.pdf#concreteprfs>
fn prf_ock(ovk: [u8; 32], cv: [u8; 32], cm_u: [u8; 32], ephemeral_key: [u8; 32]) -> [u8; 32] {
    let hash = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"Zcash_Derive_ock")
        .to_state()
        .update(&ovk)
        .update(&cv)
        .update(&cm_u)
        .update(&ephemeral_key)
        .finalize();

    <[u8; 32]>::try_from(hash.as_bytes()).expect("32 byte array")
}

/// Invokes Blake2s-256 as _CRH^ivk_, to derive the IncomingViewingKey
/// bytes from an AuthorizingKey and NullifierDerivingKey.
///
/// _CRH^ivk(ak, nk) := BLAKE2s-256("Zcashivk", ak || nk)_
///
/// <https://zips.z.cash/protocol/protocol.pdf#concretecrhivk>
fn crh_ivk(ak: [u8; 32], nk: [u8; 32]) -> [u8; 32] {
    let hash = blake2s_simd::Params::new()
        .hash_length(32)
        .personal(b"Zcashivk")
        .to_state()
        .update(&ak[..])
        .update(&nk[..])
        .finalize();

    *hash.as_array()
}

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

/// FindGroupHash for JubJub, from [zcash_primitives][0]
///
/// d is an 8-byte domain separator ("personalization"), m is the hash
/// input.
///
/// [0]: https://github.com/zcash/librustzcash/blob/master/zcash_primitives/src/jubjub/mod.rs#L409
/// <https://zips.z.cash/protocol/protocol.pdf#concretegrouphashjubjub>
// TODO: move common functions like these out of the keys module into
// a more appropriate location
pub(super) fn find_group_hash(d: [u8; 8], m: &[u8]) -> jubjub::ExtendedPoint {
    let mut tag = m.to_vec();
    let i = tag.len();
    tag.push(0u8);

    loop {
        let gh = jubjub_group_hash(d, &tag[..]);

        // We don't want to overflow and start reusing generators
        assert!(tag[i] != u8::max_value());
        tag[i] += 1;

        if let Some(gh) = gh {
            break gh;
        }
    }
}

/// Instance of FindGroupHash for JubJub, using personalized by
/// BLAKE2s for picking the proof generation key base point.
///
/// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
fn zcash_h() -> jubjub::ExtendedPoint {
    find_group_hash(*b"Zcash_H_", b"")
}

/// Used to derive a diversified base point from a diversifier value.
///
/// <https://zips.z.cash/protocol/protocol.pdf#concretediversifyhash>
fn diversify_hash(d: [u8; 11]) -> Option<jubjub::ExtendedPoint> {
    jubjub_group_hash(*b"Zcash_gd", &d)
}

// TODO: replace with reference to redjubjub or jubjub when merged and
// exported.
type Scalar = jubjub::Fr;

/// Magic human-readable strings used to identify what networks
/// Sapling Spending Keys are associated with when encoded/decoded
/// with bech32.
mod sk_hrp {
    pub const MAINNET: &str = "secret-spending-key-main";
    pub const TESTNET: &str = "secret-spending-key-test";
}

/// A _Spending Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Our root secret key of the Sapling key derivation tree. All other
/// Sapling key types derive from the `SpendingKey` value.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Debug)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct SpendingKey {
    network: Network,
    bytes: [u8; 32],
}

impl SpendingKey {
    /// Generate a new _SpendingKey_.
    pub fn new<T>(csprng: &mut T) -> Self
    where
        T: RngCore + CryptoRng,
    {
        let mut bytes = [0u8; 32];
        csprng.fill_bytes(&mut bytes);

        Self::from(bytes)
    }
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

impl Eq for SpendingKey {}

// TODO: impl a From that accepts a Network?

impl From<[u8; 32]> for SpendingKey {
    /// Generate a `SpendingKey` from existing bytes.
    fn from(bytes: [u8; 32]) -> Self {
        Self {
            network: Network::default(),
            bytes,
        }
    }
}

impl fmt::Display for SpendingKey {
    #[allow(clippy::unwrap_in_result)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => sk_hrp::MAINNET,
            _ => sk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.bytes.to_base32(), Variant::Bech32)
            .expect("hrp is valid")
    }
}

impl FromStr for SpendingKey {
    type Err = SerializationError;

    #[allow(clippy::unwrap_in_result)]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let decoded =
                    Vec::<u8>::from_base32(&bytes).expect("bech32::decode guarantees valid base32");

                let mut decoded_bytes = [0u8; 32];
                decoded_bytes[..].copy_from_slice(&decoded[0..32]);

                Ok(SpendingKey {
                    network: match hrp.as_str() {
                        sk_hrp::MAINNET => Network::Mainnet,
                        _ => Network::Testnet,
                    },
                    bytes: decoded_bytes,
                })
            }
            _ => Err(SerializationError::Parse("bech32 decoding error")),
        }
    }
}

impl PartialEq for SpendingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

/// A _Spend Authorizing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used to generate _spend authorization randomizers_ to sign each
/// _Spend Description_, proving ownership of notes.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone)]
pub struct SpendAuthorizingKey(pub(crate) Scalar);

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

impl Eq for SpendAuthorizingKey {}

impl From<SpendAuthorizingKey> for [u8; 32] {
    fn from(sk: SpendAuthorizingKey) -> Self {
        sk.0.to_bytes()
    }
}

impl From<SpendingKey> for SpendAuthorizingKey {
    /// Invokes Blake2b-512 as _PRF^expand_, t=0, to derive a
    /// SpendAuthorizingKey from a SpendingKey.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
    fn from(spending_key: SpendingKey) -> SpendAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.bytes, &[0]);

        Self(Scalar::from_bytes_wide(&hash_bytes))
    }
}

impl PartialEq for SpendAuthorizingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for SpendAuthorizingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_bytes().ct_eq(other).unwrap_u8() == 1u8
    }
}

/// A _Proof Authorizing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used in the _Spend Statement_ to prove nullifier integrity.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone)]
pub struct ProofAuthorizingKey(pub(crate) Scalar);

impl ConstantTimeEq for ProofAuthorizingKey {
    /// Check whether two `ProofAuthorizingKey`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.to_bytes().ct_eq(&other.0.to_bytes())
    }
}

impl fmt::Debug for ProofAuthorizingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ProofAuthorizingKey")
            .field(&hex::encode(<[u8; 32]>::from(*self)))
            .finish()
    }
}

impl Eq for ProofAuthorizingKey {}

impl From<ProofAuthorizingKey> for [u8; 32] {
    fn from(nsk: ProofAuthorizingKey) -> Self {
        nsk.0.to_bytes()
    }
}

impl From<SpendingKey> for ProofAuthorizingKey {
    /// For this invocation of Blake2b-512 as _PRF^expand_, t=1.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
    fn from(spending_key: SpendingKey) -> ProofAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.bytes, &[1]);

        Self(Scalar::from_bytes_wide(&hash_bytes))
    }
}

impl PartialEq for ProofAuthorizingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for ProofAuthorizingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_bytes().ct_eq(other).unwrap_u8() == 1u8
    }
}

/// An _Outgoing Viewing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used to decrypt outgoing notes without spending them.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct OutgoingViewingKey(pub(crate) [u8; 32]);

impl fmt::Debug for OutgoingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("OutgoingViewingKey")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl From<[u8; 32]> for OutgoingViewingKey {
    /// Generate an _OutgoingViewingKey_ from existing bytes.
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<OutgoingViewingKey> for [u8; 32] {
    fn from(ovk: OutgoingViewingKey) -> [u8; 32] {
        ovk.0
    }
}

impl From<SpendingKey> for OutgoingViewingKey {
    /// For this invocation of Blake2b-512 as _PRF^expand_, t=2.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
    fn from(spending_key: SpendingKey) -> OutgoingViewingKey {
        let hash_bytes = prf_expand(spending_key.bytes, &[2]);

        let mut bytes = [0u8; 32];
        bytes[..].copy_from_slice(&hash_bytes[0..32]);

        Self(bytes)
    }
}

impl PartialEq<[u8; 32]> for OutgoingViewingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0 == *other
    }
}

/// An _Authorizing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used to validate _Spend Authorization Signatures_, proving
/// ownership of notes.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Debug)]
pub struct AuthorizingKey(pub(crate) redjubjub::VerificationKey<SpendAuth>);

impl Eq for AuthorizingKey {}

impl TryFrom<[u8; 32]> for AuthorizingKey {
    type Error = &'static str;

    /// Convert an array into an AuthorizingKey.
    ///
    /// Returns an error if the encoding is malformed or if [it does not encode
    /// a prime-order point][1]:
    ///
    /// > When decoding this representation, the key MUST be considered invalid
    /// > if abst_J returns ⊥ for either ak or nk, or if ak not in J^{(r)*}
    ///
    /// [1]: https://zips.z.cash/protocol/protocol.pdf#saplingfullviewingkeyencoding
    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let affine_point = jubjub::AffinePoint::from_bytes(bytes);
        if affine_point.is_none().into() {
            return Err("Invalid jubjub::AffinePoint for Sapling AuthorizingKey");
        }
        if affine_point.unwrap().is_prime_order().into() {
            Ok(Self(redjubjub::VerificationKey::try_from(bytes).map_err(
                |_e| "Invalid jubjub::AffinePoint for Sapling AuthorizingKey",
            )?))
        } else {
            Err("jubjub::AffinePoint value for Sapling AuthorizingKey is not of prime order")
        }
    }
}

impl From<AuthorizingKey> for [u8; 32] {
    fn from(ak: AuthorizingKey) -> [u8; 32] {
        ak.0.into()
    }
}

impl From<SpendAuthorizingKey> for AuthorizingKey {
    fn from(ask: SpendAuthorizingKey) -> Self {
        let sk = redjubjub::SigningKey::<SpendAuth>::try_from(<[u8; 32]>::from(ask)).expect(
            "a scalar converted to byte array and then converted back to a scalar should not fail",
        );
        Self(redjubjub::VerificationKey::from(&sk))
    }
}

impl PartialEq for AuthorizingKey {
    fn eq(&self, other: &Self) -> bool {
        self == &<[u8; 32]>::from(*other)
    }
}

impl PartialEq<[u8; 32]> for AuthorizingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        &<[u8; 32]>::from(self.0) == other
    }
}

/// A _Nullifier Deriving Key_, as described in [protocol
/// specification §4.2.2][ps].
///
/// Used to create a `Nullifier` per note.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone)]
pub struct NullifierDerivingKey(pub(crate) jubjub::AffinePoint);

impl ConstantTimeEq for NullifierDerivingKey {
    /// Check whether two `NullifierDerivingKey`s are equal, runtime independent
    /// of the value of the secret.
    fn ct_eq(&self, other: &Self) -> Choice {
        self.0.to_bytes().ct_eq(&other.0.to_bytes())
    }
}

impl fmt::Debug for NullifierDerivingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NullifierDerivingKey")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<[u8; 32]> for NullifierDerivingKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
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

impl From<ProofAuthorizingKey> for NullifierDerivingKey {
    /// Requires JubJub's _FindGroupHash^J("Zcash_H_", "")_, then uses
    /// the resulting generator point to scalar multiply the
    /// ProofAuthorizingKey into the new NullifierDerivingKey
    ///
    /// <https://github.com/zcash/librustzcash/blob/master/zcash_primitives/src/group_hash.rs>
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concretegrouphashjubjub>
    fn from(nsk: ProofAuthorizingKey) -> Self {
        // Should this point, when generated, be fixed for the rest of
        // the protocol instance? Since this is kind of hash-and-pray, it
        // seems it might not always return the same result?
        let generator_point = zcash_h();

        // TODO: impl Mul<ExtendedPoint> for Fr, so we can reverse
        // this to match the math in the spec / general scalar mult
        // notation convention.
        Self(jubjub::AffinePoint::from(generator_point * nsk.0))
    }
}

impl PartialEq for NullifierDerivingKey {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other).unwrap_u8() == 1u8
    }
}

impl PartialEq<[u8; 32]> for NullifierDerivingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.0.to_bytes().ct_eq(other).unwrap_u8() == 1u8
    }
}

/// Magic human-readable strings used to identify what networks
/// Sapling IncomingViewingKeys are associated with when
/// encoded/decoded with bech32.
mod ivk_hrp {
    pub const MAINNET: &str = "zivks";
    pub const TESTNET: &str = "zivktestsapling";
}

/// An _Incoming Viewing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used to decrypt incoming notes without spending them.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct IncomingViewingKey {
    network: Network,
    scalar: Scalar,
}

// TODO: impl a From that accepts a Network?

impl fmt::Debug for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("IncomingViewingKey")
            .field(&hex::encode(self.scalar.to_bytes()))
            .finish()
    }
}

impl fmt::Display for IncomingViewingKey {
    #[allow(clippy::unwrap_in_result)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => ivk_hrp::MAINNET,
            _ => ivk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.scalar.to_bytes().to_base32(), Variant::Bech32)
            .expect("hrp is valid")
    }
}

impl From<[u8; 32]> for IncomingViewingKey {
    /// Generate an _IncomingViewingKey_ from existing bytes.
    fn from(mut bytes: [u8; 32]) -> Self {
        // Drop the most significant five bits, so it can be interpreted
        // as a scalar.
        //
        // I don't want to put this inside crh_ivk, but does it belong
        // inside Scalar/Fr::from_bytes()? That seems the better
        // place...
        //
        // https://github.com/zcash/librustzcash/blob/master/zcash_primitives/src/primitives.rs#L86
        bytes[31] &= 0b0000_0111;

        Self {
            // TODO: handle setting the Network better.
            network: Network::default(),
            scalar: Scalar::from_bytes(&bytes).unwrap(),
        }
    }
}

impl From<(AuthorizingKey, NullifierDerivingKey)> for IncomingViewingKey {
    /// For this invocation of Blake2s-256 as _CRH^ivk_.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
    /// <https://zips.z.cash/protocol/protocol.pdf#jubjub>
    // TODO: return None if ivk = 0
    //
    // "If ivk = 0, discard this key and start over with a new
    // [spending key]." - [§4.2.2][ps]
    //
    // [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    fn from((ask, nk): (AuthorizingKey, NullifierDerivingKey)) -> Self {
        let hash_bytes = crh_ivk(ask.into(), nk.into());

        IncomingViewingKey::from(hash_bytes)
    }
}

impl FromStr for IncomingViewingKey {
    type Err = SerializationError;

    #[allow(clippy::unwrap_in_result)]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let decoded =
                    Vec::<u8>::from_base32(&bytes).expect("bech32::decode guarantees valid base32");

                let mut scalar_bytes = [0u8; 32];
                scalar_bytes[..].copy_from_slice(&decoded[0..32]);

                Ok(IncomingViewingKey {
                    network: match hrp.as_str() {
                        ivk_hrp::MAINNET => Network::Mainnet,
                        _ => Network::Testnet,
                    },
                    scalar: Scalar::from_bytes(&scalar_bytes).unwrap(),
                })
            }
            _ => Err(SerializationError::Parse("bech32 decoding error")),
        }
    }
}

impl PartialEq<[u8; 32]> for IncomingViewingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        self.scalar.to_bytes() == *other
    }
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
            .field(&hex::encode(&self.0))
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

impl From<SpendingKey> for Diversifier {
    /// Derives a [_default diversifier_][4.2.2] from a SpendingKey.
    ///
    /// 'For each spending key, there is also a default diversified
    /// payment address with a "random-looking" diversifier. This
    /// allows an implementation that does not expose diversified
    /// addresses as a user-visible feature, to use a default address
    /// that cannot be distinguished (without knowledge of the
    /// spending key) from one with a random diversifier...'
    ///
    /// [4.2.2]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    fn from(sk: SpendingKey) -> Diversifier {
        let mut i = 0u8;

        loop {
            let mut d_bytes = [0u8; 11];
            d_bytes[..].copy_from_slice(&prf_expand(sk.bytes, &[3, i])[..11]);

            if diversify_hash(d_bytes).is_some() {
                break Self(d_bytes);
            }

            assert!(i < 255);

            i += 1;
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
    pub fn new<T>(csprng: &mut T) -> Self
    where
        T: RngCore + CryptoRng,
    {
        loop {
            let mut bytes = [0u8; 11];
            csprng.fill_bytes(&mut bytes);

            if diversify_hash(bytes).is_some() {
                break Self(bytes);
            }
        }
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

impl TryFrom<(IncomingViewingKey, Diversifier)> for TransmissionKey {
    type Error = &'static str;

    /// This includes _KA^Sapling.DerivePublic(ivk, G_d)_, which is just a
    /// scalar mult _\[ivk\]G_d_.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement>
    fn try_from((ivk, d): (IncomingViewingKey, Diversifier)) -> Result<Self, Self::Error> {
        let affine_point = jubjub::AffinePoint::from(
            diversify_hash(d.0).ok_or("invalid diversifier")? * ivk.scalar,
        );
        // We need to ensure that the result point is in the prime-order subgroup.
        // Since diversify_hash() returns a point in the prime-order subgroup,
        // then the result point will also be in the prime-order subgroup
        // and there is no need to check anything.
        Ok(Self(affine_point))
    }
}

impl PartialEq<[u8; 32]> for TransmissionKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        &self.0.to_bytes() == other
    }
}

/// Magic human-readable strings used to identify what networks
/// Sapling FullViewingKeys are associated with when encoded/decoded
/// with bech32.
mod fvk_hrp {
    pub const MAINNET: &str = "zviews";
    pub const TESTNET: &str = "zviewtestsapling";
}

/// Full Viewing Keys
///
/// Allows recognizing both incoming and outgoing notes without having
/// spend authority.
///
/// For incoming viewing keys on the production network, the
/// Human-Readable Part is "zviews". For incoming viewing keys on the
/// test network, the Human-Readable Part is "zviewtestsapling".
///
/// <https://zips.z.cash/protocol/protocol.pdf#saplingfullviewingkeyencoding>
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct FullViewingKey {
    network: Network,
    authorizing_key: AuthorizingKey,
    nullifier_deriving_key: NullifierDerivingKey,
    outgoing_viewing_key: OutgoingViewingKey,
}

// TODO: impl a From that accepts a Network?

impl fmt::Debug for FullViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FullViewingKey")
            .field("network", &self.network)
            .field("authorizing_key", &self.authorizing_key)
            .field("nullifier_deriving_key", &self.nullifier_deriving_key)
            .field("outgoing_viewing_key", &self.outgoing_viewing_key)
            .finish()
    }
}

impl fmt::Display for FullViewingKey {
    #[allow(clippy::unwrap_in_result)]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = bytes.write_all(&<[u8; 32]>::from(self.authorizing_key));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.nullifier_deriving_key));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.outgoing_viewing_key));

        let hrp = match self.network {
            Network::Mainnet => fvk_hrp::MAINNET,
            _ => fvk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, bytes.get_ref().to_base32(), Variant::Bech32)
            .expect("hrp is valid")
    }
}

impl FromStr for FullViewingKey {
    type Err = SerializationError;

    #[allow(clippy::unwrap_in_result)]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let mut decoded_bytes = io::Cursor::new(
                    Vec::<u8>::from_base32(&bytes).expect("bech32::decode guarantees valid base32"),
                );

                let authorizing_key_bytes = decoded_bytes.read_32_bytes()?;
                let nullifier_deriving_key_bytes = decoded_bytes.read_32_bytes()?;
                let outgoing_key_bytes = decoded_bytes.read_32_bytes()?;

                Ok(FullViewingKey {
                    network: match hrp.as_str() {
                        fvk_hrp::MAINNET => Network::Mainnet,
                        _ => Network::Testnet,
                    },
                    authorizing_key: AuthorizingKey::try_from(authorizing_key_bytes)
                        .map_err(SerializationError::Parse)?,
                    nullifier_deriving_key: NullifierDerivingKey::from(
                        nullifier_deriving_key_bytes,
                    ),
                    outgoing_viewing_key: OutgoingViewingKey::from(outgoing_key_bytes),
                })
            }
            _ => Err(SerializationError::Parse("bech32 decoding error")),
        }
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
