//! Sapling key types
//!
//! "The spend authorizing key ask, proof authorizing key (ak, nsk),
//! full viewing key (ak, nk, ovk), incoming viewing key ivk, and each
//! diversified payment address addr_d = (d, pk_d ) are derived from sk,
//! as described in [Sapling Key Components][ps]." - [§3.1][3.1]
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
//! [3.1]: https://zips.z.cash/protocol/protocol.pdf#addressesandkeys

#[cfg(test)]
mod test_vectors;
#[cfg(test)]
mod tests;

use std::{
    convert::{From, Into, TryFrom},
    fmt,
    io::{self, Write},
    ops::Deref,
};

use bech32::{self, FromBase32, ToBase32};
use blake2b_simd;
use blake2s_simd;
use jubjub;
use rand_core::{CryptoRng, RngCore};
use redjubjub::{self, SpendAuth};

// #[cfg(test)]
// use proptest::{array, prelude::*};
#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::{
    serialization::{ReadZcashExt, SerializationError},
    Network,
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
pub const RANDOMNESS_BEACON_URS: &[u8; 64] =
    b"096b36a5804bfacef1691e173c366a47ff5ba84a44f26ddd7e8d9f79d5b42df0";

/// Invokes Blake2b-512 as PRF^expand with parameter t, to derive a
/// SpendAuthorizingKey and ProofAuthorizingKey from SpendingKey.
///
/// PRF^expand(sk, t) := BLAKE2b-512("Zcash_ExpandSeed", sk || t)
///
/// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
fn prf_expand(sk: [u8; 32], t: u8) -> [u8; 64] {
    let hash = blake2b_simd::Params::new()
        .hash_length(64)
        .personal(b"Zcash_ExpandSeed")
        .to_state()
        .update(&sk[..])
        .update(&[t])
        .finalize();

    *hash.as_array()
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
fn find_group_hash(d: [u8; 8], m: &[u8]) -> jubjub::ExtendedPoint {
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
/// Sprout key types derive from the SpendingKey value.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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

        bech32::encode_to_fmt(f, hrp, &self.bytes.to_base32()).unwrap()
    }
}

impl std::str::FromStr for SpendingKey {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes)) => {
                let decoded = Vec::<u8>::from_base32(&bytes).unwrap();

                let mut decoded_bytes = [0u8; 32];
                decoded_bytes[..].copy_from_slice(&decoded[0..32]);

                Ok(SpendingKey {
                    network: match hrp.as_str() {
                        ivk_hrp::MAINNET => Network::Mainnet,
                        _ => Network::Testnet,
                    },
                    bytes: decoded_bytes,
                })
            }
            Err(_) => Err(SerializationError::Parse("bech32 decoding error")),
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
pub struct SpendAuthorizingKey(pub Scalar);

impl Deref for SpendAuthorizingKey {
    type Target = Scalar;

    fn deref(&self) -> &Scalar {
        &self.0
    }
}

impl fmt::Debug for SpendAuthorizingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("SpendAuthorizingKey")
            .field(&hex::encode(self.to_bytes()))
            .finish()
    }
}

impl From<SpendingKey> for SpendAuthorizingKey {
    /// Invokes Blake2b-512 as _PRF^expand_, t=0, to derive a
    /// SpendAuthorizingKey from a SpendingKey.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> SpendAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.bytes, 0);

        Self(Scalar::from_bytes_wide(&hash_bytes))
    }
}

/// A _Proof Authorizing Key_, as described in [protocol specification
/// §4.2.2][ps].
///
/// Used in the _Spend Statement_ to prove nullifier integrity.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
pub struct ProofAuthorizingKey(pub Scalar);

impl Deref for ProofAuthorizingKey {
    type Target = Scalar;

    fn deref(&self) -> &Scalar {
        &self.0
    }
}

impl fmt::Debug for ProofAuthorizingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ProofAuthorizingKey")
            .field(&hex::encode(&self.to_bytes()))
            .finish()
    }
}

impl From<SpendingKey> for ProofAuthorizingKey {
    /// For this invocation of Blake2b-512 as _PRF^expand_, t=1.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> ProofAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.bytes, 1);

        Self(Scalar::from_bytes_wide(&hash_bytes))
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
    fn from(nk: OutgoingViewingKey) -> [u8; 32] {
        nk.0
    }
}

impl From<SpendingKey> for OutgoingViewingKey {
    /// For this invocation of Blake2b-512 as _PRF^expand_, t=2.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> OutgoingViewingKey {
        let hash_bytes = prf_expand(spending_key.bytes, 2);

        let mut bytes = [0u8; 32];
        bytes[..].copy_from_slice(&hash_bytes[0..32]);

        Self(bytes)
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
pub struct AuthorizingKey(pub redjubjub::PublicKey<SpendAuth>);

impl Deref for AuthorizingKey {
    type Target = redjubjub::PublicKey<SpendAuth>;

    fn deref(&self) -> &redjubjub::PublicKey<SpendAuth> {
        &self.0
    }
}

impl From<[u8; 32]> for AuthorizingKey {
    fn from(bytes: [u8; 32]) -> Self {
        let sk = redjubjub::SecretKey::<SpendAuth>::try_from(bytes).unwrap();
        Self(redjubjub::PublicKey::from(&sk))
    }
}

impl From<AuthorizingKey> for [u8; 32] {
    fn from(ak: AuthorizingKey) -> [u8; 32] {
        ak.0.into()
    }
}

impl From<SpendAuthorizingKey> for AuthorizingKey {
    fn from(ask: SpendAuthorizingKey) -> Self {
        let sk = redjubjub::SecretKey::<SpendAuth>::try_from(ask.to_bytes()).unwrap();
        Self(redjubjub::PublicKey::from(&sk))
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

impl From<[u8; 32]> for NullifierDerivingKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl From<NullifierDerivingKey> for [u8; 32] {
    fn from(nk: NullifierDerivingKey) -> [u8; 32] {
        nk.0.to_bytes()
    }
}

impl Deref for NullifierDerivingKey {
    type Target = jubjub::AffinePoint;

    fn deref(&self) -> &jubjub::AffinePoint {
        &self.0
    }
}

impl fmt::Debug for NullifierDerivingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NullifierDerivingKey")
            .field("u", &hex::encode(self.get_u().to_bytes()))
            .field("v", &hex::encode(self.get_v().to_bytes()))
            .finish()
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

// TODO: impl a top-level to_bytes or PartialEq between this and [u8; 32]

// TODO: impl a From that accepts a Network?

impl Deref for IncomingViewingKey {
    type Target = Scalar;

    fn deref(&self) -> &Scalar {
        &self.scalar
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
            network: Network::default(),
            scalar: Scalar::from_bytes(&bytes).unwrap(),
        }
    }
}

impl From<IncomingViewingKey> for [u8; 32] {
    fn from(ivk: IncomingViewingKey) -> [u8; 32] {
        ivk.scalar.to_bytes()
    }
}

impl fmt::Debug for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("IncomingViewingKey")
            .field(&hex::encode(self.to_bytes()))
            .finish()
    }
}

impl fmt::Display for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hrp = match self.network {
            Network::Mainnet => ivk_hrp::MAINNET,
            _ => ivk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, &self.scalar.to_bytes().to_base32()).unwrap()
    }
}

impl std::str::FromStr for IncomingViewingKey {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes)) => {
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
            Err(_) => Err(SerializationError::Parse("bech32 decoding error")),
        }
    }
}

impl IncomingViewingKey {
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
    //
    // TODO: won't let me name this `from(arg1, arg2)` when I have From impl'd above?
    pub fn from_keys(
        authorizing_key: AuthorizingKey,
        nullifier_deriving_key: NullifierDerivingKey,
    ) -> Self {
        let hash_bytes = crh_ivk(authorizing_key.into(), nullifier_deriving_key.to_bytes());

        IncomingViewingKey::from(hash_bytes)
    }
}

/// A _Diversifier_, as described in [protocol specification §4.2.2][ps].
///
/// Combined with an _IncomingViewingKey_, produces a _diversified
/// payment address_.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct Diversifier(pub [u8; 11]);

// TODO: _DefaultDiversifier_

impl fmt::Debug for Diversifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Diversifier")
            .field(&hex::encode(&self.0))
            .finish()
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
        // Is this loop overkill?
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

impl Eq for TransmissionKey {}

impl Deref for TransmissionKey {
    type Target = jubjub::AffinePoint;

    fn deref(&self) -> &jubjub::AffinePoint {
        &self.0
    }
}

impl fmt::Debug for TransmissionKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("TransmissionKey")
            .field("u", &hex::encode(self.get_u().to_bytes()))
            .field("v", &hex::encode(self.get_v().to_bytes()))
            .finish()
    }
}

impl TransmissionKey {
    /// This includes _KA^Sapling.DerivePublic(ivk, G_d)_, which is just a
    /// scalar mult _[ivk]G_d_.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concretesaplingkeyagreement
    pub fn from(ivk: IncomingViewingKey, d: Diversifier) -> Self {
        Self(jubjub::AffinePoint::from(
            diversify_hash(d.0).unwrap() * ivk.scalar,
        ))
    }

    /// Attempts to interpret a byte representation of an
    /// affine point, failing if the element is not on
    /// the curve or non-canonical.
    ///
    /// https://github.com/zkcrypto/jubjub/blob/master/src/lib.rs#L411
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(b).unwrap())
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
/// Human-Readable Part is “zviews”. For incoming viewing keys on the
/// test network, the Human-Readable Part is “zviewtestsapling”.
///
/// https://zips.z.cash/protocol/protocol.pdf#saplingfullviewingkeyencoding
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

        let auth_key_bytes: [u8; 32] = self.authorizing_key.into();

        let _ = bytes.write_all(&auth_key_bytes);
        let _ = bytes.write_all(&self.nullifier_deriving_key.to_bytes());
        let _ = bytes.write_all(&self.outgoing_viewing_key.0);

        let hrp = match self.network {
            Network::Mainnet => fvk_hrp::MAINNET,
            _ => fvk_hrp::TESTNET,
        };

        bech32::encode_to_fmt(f, hrp, bytes.get_ref().to_base32()).unwrap()
    }
}

impl std::str::FromStr for FullViewingKey {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match bech32::decode(s) {
            Ok((hrp, bytes)) => {
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
            Err(_) => Err(SerializationError::Parse("bech32 decoding error")),
        }
    }
}
