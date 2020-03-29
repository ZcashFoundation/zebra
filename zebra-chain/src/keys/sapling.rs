//! Sapling key types
//!
//! "The spend authorizing key ask, proof authorizing key (ak, nsk),
//! full viewing key (ak, nk, ovk), incoming viewing key ivk, and each
//! diversified payment address addr_d = (d, pk_d ) are derived from sk,
//! as described in [Sapling Key Components][ps]."
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents

use std::fmt;

use blake2b_simd;
use jubjub;
use rand_core::{CryptoRng, RngCore};

#[cfg(test)]
use proptest::prelude::*;
#[cfg(test)]
use proptest_derive::Arbitrary;

// TODO: replace with reference to redjubjub or jubjub when merged and
// exported.
type Scalar = jubjub::Fr;

/// Our root secret key of the Sprout key derivation tree.
///
/// All other Sprout key types derive from the SpendingKey value.
///
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpendingKey(pub [u8; 32]);

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

impl From<[u8; 32]> for SpendingKey {
    /// Generate a _SpendingKey_ from existing bytes.
    fn from(mut bytes: [u8; 32]) -> SpendingKey {
        SpendingKey(bytes)
    }
}

/// Derived from a _SpendingKey_.
pub type SpendAuthorizationKey = Scalar;

impl From<SpendingKey> for SpendAuthorizationKey {
    /// Invokes Blake2b-512 as PRF^expand, t=0, to derive a
    /// SpendAuthorizationKey from a SpendingKey.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> SpendAuthorizationKey {
        let hash = blake2b_simd::Params::new()
            .hash_length(64) // Blake2b-512
            .personal(b"Zcash_ExpandSeed")
            .to_state()
            .update(spending_key.0[..])
            .update([0]) // t=0
            .finalize();

        Self::from(hash)
    }
}

/// Derived from a _SpendingKey_.
pub type ProofAuthorizingKey = Scalar;

impl fmt::Debug for ProofAuthorizingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ProofAuthorizingKey")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl From<SpendingKey> for ProofAuthorizingKey {
    /// For this invocation of Blake2b-512 as PRF^expand, t=1.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> ProofAuthorizingKey {
        let hash = blake2b_simd::Params::new()
            .hash_length(64)
            .personal(b"Zcash_ExpandSeed")
            .to_state()
            .update(spending_key.0[..])
            .update([1])
            .finalize();

        Self::from(hash)
    }
}

/// Derived from a _SpendingKey_.
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct OutgoingViewingKey(pub [u8; 32]);

impl fmt::Debug for OutgoingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("OutgoingViewingKey")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl From<SpendingKey> for OutgoingViewingKey {
    /// For this invocation of Blake2b-512 as PRF^expand, t=2.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> OutgoingViewingKey {
        let hash = blake2b_simd::Params::new()
            .hash_length(64)
            .personal(b"Zcash_ExpandSeed")
            .to_state()
            .update(spending_key.0[..])
            .update([2])
            .finalize();

        Self::from(hash[0..32])
    }
}

///
pub type AuthorizingKey = redjubjub::PublicKeyBytes;

impl fmt::Debug for AuthorizingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("AuthorizingKey")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

///
pub type NullifierDerivingKey = redjubjub::PublicKeyBytes;

///
pub type IncomingViewingKey = Scalar;

///
#[derive(Copy, Clone, Display, Eq, PartialEq)]
pub struct Diversifier(pub [u8; 11]);

impl fmt::Debug for Diversifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Diversifier")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

/// A (diversified) _TransmissionKey_
///
/// In Sapling, secrets need to be transmitted to a recipient of funds
/// in order for them to be later spent. To transmit these secrets
/// securely to a recipient without requiring an out-of-band
/// communication channel, the diversied transmission key is used to
/// encrypt them.
///
/// Derived by multiplying a JubJub point [derived][ps] from a
/// _Diversifier_ by the _IncomingViewingKey_ scalar.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#concretediversifyhash
pub type TransmissionKey = redjubjub::PublicKeyBytes;

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
    authorizing_key: AuthorizingKey,
    nullifier_deriving_key: NullifierDerivingKey,
    outgoing_viewing_key: OutgoingViewingKey,
}

#[cfg(test)]
mod tests {

    use rand_core::OsRng;

    use super::*;

    // #[test]
    // TODO: test vectors, not just random data
    // fn derive_keys() {
    // }
}

#[cfg(test)]
proptest! {

    //#[test]
    // fn test() {}
}
