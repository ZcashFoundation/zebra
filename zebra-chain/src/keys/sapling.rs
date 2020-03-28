//! Sapling key types
//!
//! "The spend authorizing key ask, proof authorizing key (ak, nsk),
//! full viewing key (ak, nk, ovk), incoming viewing key ivk, and each
//! diversified payment address addr_d = (d, pk_d ) are derived from sk,
//! as described in [Sapling Key Components][ps]."
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents

use std::fmt;

use blake2::{Blake2b, Digest};
use rand_core::{CryptoRng, RngCore};

#[cfg(test)]
use proptest::prelude::*;
#[cfg(test)]
use proptest_derive::Arbitrary;

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
pub type SpendAuthorizationKey = redjubjub::Scalar;

impl From<SpendingKey> for SpendAuthorizationKey {
    /// Invokes Blake2b-512 as PRF^expand to derive a
    /// SpendAuthorizationKey from a SpendingKey.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> SpendAuthorizationKey {
        let mut block = [0u8; 33]; // Last byte is t=0;

        block[0..32].copy_from_slice(&spending_key.0[..]);

        let mut hasher = Blake2b::new();
        // TODO: check that this counts as personalization.
        hasher.input("Zcash_ExpandSeed");
        hasher.input(block);

        Self::from(hasher.result())
    }
}

/// Derived from a _SpendingKey_.
pub type ProofAuthorizingKey = redjubjub::Scalar;

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
        let mut block = [0u8; 33];
        block[33] = 1; // Last byte is t=1;

        block[0..32].copy_from_slice(&spending_key.0[..]);

        let mut hasher = Blake2b::new();
        // TODO: check that this counts as personalization.
        hasher.input("Zcash_ExpandSeed");
        hasher.input(block);

        Self::from(hasher.result())
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
    /// For this invocation of Blake2b-512 as PRF^expand, t=1.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> OutgoingViewingKey {
        let mut block = [0u8; 33];
        block[33] = 2u8; // Last byte is t=2;

        block[0..32].copy_from_slice(&spending_key.0[..]);

        let mut hasher = Blake2b::new();
        // TODO: check that this counts as personalization.
        hasher.input("Zcash_ExpandSeed");
        hasher.input(block);

        Self(hasher.result()[0..31])
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
pub type IncomingViewingKey = redjubjub::Scalar;

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

/// Derived from a _IncomingViewingKey_ and a _Diversifier_
///
/// Derived by multiplying the diversifier (converted to an affine
/// point on the Jubjub curve) by the incoming view key:
/// pkd = gd * ivk
pub type TransmissionKey = redjubjub::PublicKeyBytes;

/// Full Viewing Keys
///
/// For incoming viewing keys on the production network, the
/// Human-Readable Part is “zviews”. For incoming viewing keys on the
/// test network, the Human-Readable Part is “zviewtestsapling”.
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
