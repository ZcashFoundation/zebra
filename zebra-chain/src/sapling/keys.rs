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
/// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
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
/// https://zips.z.cash/protocol/nu5.pdf#concreteprfs
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

    *hash.as_bytes().try_into().expect("32 byte array")
}

/// Invokes Blake2s-256 as _CRH^ivk_, to derive the IncomingViewingKey
/// bytes from an AuthorizingKey and NullifierDerivingKey.
///
/// _CRH^ivk(ak, nk) := BLAKE2s-256("Zcashivk", ak || nk)_
///
/// https://zips.z.cash/protocol/protocol.pdf#concretecrhivk
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
/// https://zips.z.cash/protocol/protocol.pdf#concretegrouphashjubjub
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
/// https://zips.z.cash/protocol/protocol.pdf#concretegrouphashjubjub
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
/// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
fn zcash_h() -> jubjub::ExtendedPoint {
    find_group_hash(*b"Zcash_H_", b"")
}

/// Used to derive a diversified base point from a diversifier value.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretediversifyhash
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
/// Sapling key types derive from the SpendingKey value.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct SpendingKey {
    network: Network,
    bytes: [u8; 32],
}

// TODO: impl a From that accepts a Network?

impl From<[u8; 32]> for SpendingKey {
    /// Generate a _SpendingKey_ from existing bytes.
    fn from(bytes: [u8; 32]) -> Self {
        Self {
            network: Network::default(),
            bytes,
        }
    }
}

impl fmt::Display for SpendingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => sk_hrp::MAINNET,
            _ => sk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.bytes.to_base32(), Variant::Bech32).unwrap()
    }
}

impl FromStr for SpendingKey {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let decoded = Vec::<u8>::from_base32(&bytes).unwrap();

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

/// A _Spend Authorizing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used to generate _spend authorization randomizers_ to sign each
/// _Spend Description_, proving ownership of notes.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct SpendAuthorizingKey(pub Scalar);

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
    /// Invokes Blake2b-512 as _PRF^expand_, t=0, to derive a
    /// SpendAuthorizingKey from a SpendingKey.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> SpendAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.bytes, &[0]);

        Self(Scalar::from_bytes_wide(&hash_bytes))
    }
}

impl PartialEq<[u8; 32]> for SpendAuthorizingKey {
    // TODO: do we want to use constant-time comparison here?
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        <[u8; 32]>::from(*self) == *other
    }
}

/// A _Proof Authorizing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used in the _Spend Statement_ to prove nullifier integrity.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct ProofAuthorizingKey(pub Scalar);

impl fmt::Debug for ProofAuthorizingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ProofAuthorizingKey")
            .field(&hex::encode(<[u8; 32]>::from(*self)))
            .finish()
    }
}

impl From<ProofAuthorizingKey> for [u8; 32] {
    fn from(nsk: ProofAuthorizingKey) -> Self {
        nsk.0.to_bytes()
    }
}

impl From<SpendingKey> for ProofAuthorizingKey {
    /// For this invocation of Blake2b-512 as _PRF^expand_, t=1.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> ProofAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.bytes, &[1]);

        Self(Scalar::from_bytes_wide(&hash_bytes))
    }
}

impl PartialEq<[u8; 32]> for ProofAuthorizingKey {
    // TODO: do we want to use constant-time comparison here?
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        <[u8; 32]>::from(*self) == *other
    }
}

/// An _Outgoing Viewing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used to decrypt outgoing notes without spending them.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct OutgoingViewingKey(pub [u8; 32]);

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
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
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
pub struct AuthorizingKey(pub redjubjub::VerificationKey<SpendAuth>);

impl Eq for AuthorizingKey {}

impl From<[u8; 32]> for AuthorizingKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(redjubjub::VerificationKey::try_from(bytes).unwrap())
    }
}

impl From<AuthorizingKey> for [u8; 32] {
    fn from(ak: AuthorizingKey) -> [u8; 32] {
        ak.0.into()
    }
}

impl From<SpendAuthorizingKey> for AuthorizingKey {
    fn from(ask: SpendAuthorizingKey) -> Self {
        let sk = redjubjub::SigningKey::<SpendAuth>::try_from(<[u8; 32]>::from(ask)).unwrap();
        Self(redjubjub::VerificationKey::from(&sk))
    }
}

impl PartialEq for AuthorizingKey {
    fn eq(&self, other: &Self) -> bool {
        <[u8; 32]>::from(self.0) == <[u8; 32]>::from(other.0)
    }
}

impl PartialEq<[u8; 32]> for AuthorizingKey {
    fn eq(&self, other: &[u8; 32]) -> bool {
        <[u8; 32]>::from(self.0) == *other
    }
}

/// A _Nullifier Deriving Key_, as described in [protocol
/// specification §4.2.2][ps].
///
/// Used to create a _Nullifier_ per note.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, PartialEq)]
pub struct NullifierDerivingKey(pub jubjub::AffinePoint);

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
    /// https://github.com/zcash/librustzcash/blob/master/zcash_primitives/src/group_hash.rs
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concretegrouphashjubjub
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

impl PartialEq<[u8; 32]> for NullifierDerivingKey {
    // TODO: do we want to use constant-time comparison here?
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        <[u8; 32]>::from(*self) == *other
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
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => ivk_hrp::MAINNET,
            _ => ivk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.scalar.to_bytes().to_base32(), Variant::Bech32).unwrap()
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
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    /// https://zips.z.cash/protocol/protocol.pdf#jubjub
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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let decoded = Vec::<u8>::from_base32(&bytes).unwrap();

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
pub struct Diversifier(pub [u8; 11]);

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
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concretediversifyhash
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
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#concretediversifyhash
#[derive(Copy, Clone, PartialEq)]
pub struct TransmissionKey(pub jubjub::AffinePoint);

impl fmt::Debug for TransmissionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("TransmissionKey")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl Eq for TransmissionKey {}

impl From<[u8; 32]> for TransmissionKey {
    /// Attempts to interpret a byte representation of an
    /// affine point, failing if the element is not on
    /// the curve or non-canonical.
    ///
    /// https://github.com/zkcrypto/jubjub/blob/master/src/lib.rs#L411
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl From<TransmissionKey> for [u8; 32] {
    fn from(pk_d: TransmissionKey) -> [u8; 32] {
        pk_d.0.to_bytes()
    }
}

impl From<(IncomingViewingKey, Diversifier)> for TransmissionKey {
    /// This includes _KA^Sapling.DerivePublic(ivk, G_d)_, which is just a
    /// scalar mult _\[ivk\]G_d_.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement
    fn from((ivk, d): (IncomingViewingKey, Diversifier)) -> Self {
        Self(jubjub::AffinePoint::from(
            diversify_hash(d.0).unwrap() * ivk.scalar,
        ))
    }
}

impl PartialEq<[u8; 32]> for TransmissionKey {
    // TODO: do we want to use constant-time comparison here?
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        <[u8; 32]>::from(*self) == *other
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
/// https://zips.z.cash/protocol/protocol.pdf#saplingfullviewingkeyencoding
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
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = bytes.write_all(&<[u8; 32]>::from(self.authorizing_key));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.nullifier_deriving_key));
        let _ = bytes.write_all(&<[u8; 32]>::from(self.outgoing_viewing_key));

        let hrp = match self.network {
            Network::Mainnet => fvk_hrp::MAINNET,
            _ => fvk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, bytes.get_ref().to_base32(), Variant::Bech32).unwrap()
    }
}

impl FromStr for FullViewingKey {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes, Variant::Bech32)) => {
                let mut decoded_bytes = io::Cursor::new(Vec::<u8>::from_base32(&bytes).unwrap());

                let authorizing_key_bytes = decoded_bytes.read_32_bytes()?;
                let nullifier_deriving_key_bytes = decoded_bytes.read_32_bytes()?;
                let outgoing_key_bytes = decoded_bytes.read_32_bytes()?;

                Ok(FullViewingKey {
                    network: match hrp.as_str() {
                        fvk_hrp::MAINNET => Network::Mainnet,
                        _ => Network::Testnet,
                    },
                    authorizing_key: AuthorizingKey::from(authorizing_key_bytes),
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

/// An ephemeral public key for Sapling key agreement.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement
#[derive(Copy, Clone, Deserialize, PartialEq, Serialize)]
pub struct EphemeralPublicKey(
    #[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint,
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
    // TODO: do we want to use constant-time comparison here?
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &[u8; 32]) -> bool {
        <[u8; 32]>::from(self) == *other
    }
}

impl TryFrom<[u8; 32]> for EphemeralPublicKey {
    type Error = &'static str;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = jubjub::AffinePoint::from_bytes(bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err("Invalid jubjub::AffinePoint value")
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
