//! Sprout nullifiers.

use serde::{Deserialize, Serialize};

/// Nullifier seed, named rho in the [spec][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents

#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct NullifierSeed(pub(crate) [u8; 32]);

impl AsRef<[u8]> for NullifierSeed {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<[u8; 32]> for NullifierSeed {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<NullifierSeed> for [u8; 32] {
    fn from(rho: NullifierSeed) -> Self {
        rho.0
    }
}

/// A Nullifier for Sprout transactions
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Nullifier(pub [u8; 32]);

impl From<[u8; 32]> for Nullifier {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(n: Nullifier) -> Self {
        n.0
    }
}

impl From<&Nullifier> for [u8; 32] {
    fn from(n: &Nullifier) -> Self {
        n.0
    }
}
