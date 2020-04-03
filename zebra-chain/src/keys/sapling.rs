//! Sapling key types
//!
//! "The spend authorizing key ask, proof authorizing key (ak, nsk),
//! full viewing key (ak, nk, ovk), incoming viewing key ivk, and each
//! diversified payment address addr_d = (d, pk_d ) are derived from sk,
//! as described in [Sapling Key Components][ps]."
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents

use std::{fmt, ops::Deref};

use blake2b_simd;
use blake2s_simd;
use jubjub;
use rand_core::{CryptoRng, RngCore};

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, array, prelude::*};

/// Invokes Blake2b-512 as PRF^expand with parameter t, to derive a
/// SpendAuthorizingKey and ProofAuthorizingKey from SpendingKey.
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
    return *hash.as_array();
}

/// Invokes Blake2s-256 as CRH^ivk, to derive the IncomingViewingKey
/// bytes from an AuthorizingKey and NullifierDerivingKey.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretecrhivk
fn crh_ivk(ak: [u8; 32], nk: [u8; 32]) -> [u8; 32] {
    let hash = blake2s_simd::Params::new()
        .hash_length(32)
        .personal(b"Zcashivk")
        .to_state()
        // TODO: double-check that `to_bytes()` == repr_J
        .update(&ak[..])
        .update(&nk[..])
        .finalize();

    return *hash.as_array();
}

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
    fn from(bytes: [u8; 32]) -> SpendingKey {
        SpendingKey(bytes)
    }
}

/// Derived from a _SpendingKey_.
pub struct SpendAuthorizationKey(Scalar);

impl Deref for SpendAuthorizationKey {
    type Target = Scalar;

    fn deref(&self) -> &Scalar {
        &self.0
    }
}

impl From<SpendingKey> for SpendAuthorizationKey {
    /// Invokes Blake2b-512 as PRF^expand, t=0, to derive a
    /// SpendAuthorizationKey from a SpendingKey.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> SpendAuthorizationKey {
        let hash_bytes = prf_expand(spending_key.0, 0);

        Self(Scalar::from_bytes_wide(&hash_bytes))
    }
}

/// Derived from a _SpendingKey_.
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
    /// For this invocation of Blake2b-512 as PRF^expand, t=1.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    fn from(spending_key: SpendingKey) -> ProofAuthorizingKey {
        let hash_bytes = prf_expand(spending_key.0, 1);

        Self(Scalar::from_bytes_wide(&hash_bytes))
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
        let hash_bytes = prf_expand(spending_key.0, 2);

        let mut bytes = [0u8; 32];
        bytes[..].copy_from_slice(&hash_bytes);

        Self(bytes)
    }
}

///
pub type AuthorizingKey = jubjub::AffinePoint;

// impl fmt::Debug for AuthorizingKey {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         f.debug_struct("AuthorizingKey")
//             .field("u", &hex::encode(&self.u))
//             .field("v", &hex::encode(&self.v))
//             .finish()
//     }
// }

///
pub type NullifierDerivingKey = jubjub::AffinePoint;

///
pub struct IncomingViewingKey(pub Scalar);

impl Deref for IncomingViewingKey {
    type Target = Scalar;

    fn deref(&self) -> &Scalar {
        &self.0
    }
}

// impl fmt::Debug for IncomingViewingKey {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         f.debug_tuple("IncomingViewingKey")
//             .field(&hex::encode(&self.0))
//             .finish()
//     }
// }

impl IncomingViewingKey {
    /// For this invocation of Blake2s-256 as CRH^ivk.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
    /// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
    /// https://zips.z.cash/protocol/protocol.pdf#jubjub
    fn new(
        authorizing_key: AuthorizingKey,
        nullifier_deriving_key: NullifierDerivingKey,
    ) -> IncomingViewingKey {
        let hash_bytes = crh_ivk(
            authorizing_key.to_bytes(),
            nullifier_deriving_key.to_bytes(),
        );

        Self(Scalar::from_bytes(&hash_bytes).unwrap())
    }
}

///
#[derive(Copy, Clone, Eq, PartialEq)]
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
pub type TransmissionKey = jubjub::AffinePoint;

// #[cfg(test)]
// impl Arbitrary for TransmissionKey {
//     type Parameters = ();

//     fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
//         (array::uniform32(any::<u8>()))
//             .prop_map(|transmission_key_bytes| {
//                 return Self::from_bytes(transmission_key_bytes).unwrap();
//             })
//             .boxed()
//     }

//     type Strategy = BoxedStrategy<Self>;
// }

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

    use super::*;

    #[test]
    fn check_deref() {
        let ivk = IncomingViewingKey(jubjub::Fr::zero());

        ivk.to_bytes();
    }
}

#[cfg(test)]
proptest! {

    //#[test]
    // fn test() {}
}
