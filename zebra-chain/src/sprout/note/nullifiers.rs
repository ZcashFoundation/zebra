//! Sprout nullifiers.

use serde::{Deserialize, Serialize};

use crate::fmt::HexDebug;

/// Nullifier seed, named rho in the [spec][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents

#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct NullifierSeed(pub(crate) HexDebug<[u8; 32]>);

impl AsRef<[u8]> for NullifierSeed {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl From<[u8; 32]> for NullifierSeed {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes.into())
    }
}

impl From<NullifierSeed> for [u8; 32] {
    fn from(rho: NullifierSeed) -> Self {
        *rho.0
    }
}

/// A Nullifier for Sprout transactions
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Nullifier(pub HexDebug<[u8; 32]>);

impl Nullifier {
    /// Return the bytes in big-endian byte order as required
    /// by RPCs such as `getrawtransaction`.
    pub fn bytes_in_display_order(&self) -> [u8; 32] {
        let mut root: [u8; 32] = self.into();
        root.reverse();
        root
    }
}

impl From<[u8; 32]> for Nullifier {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes.into())
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(n: Nullifier) -> Self {
        *n.0
    }
}

impl From<&Nullifier> for [u8; 32] {
    fn from(n: &Nullifier) -> Self {
        *n.0
    }
}
