#![allow(dead_code)]

use byteorder::{ByteOrder, LittleEndian};
use serde::{Deserialize, Serialize};
use sha2::digest::generic_array::{typenum::U64, GenericArray};

use super::super::keys::SpendingKey;

/// PRF^nf is used to derive a Sprout nullifer from the receiver's
/// spending key a_sk and a nullifier seed ρ, instantiated using the
/// SHA-256 compression function.
///
/// <https://zips.z.cash/protocol/protocol.pdf#abstractprfs>
/// <https://zips.z.cash/protocol/protocol.pdf#commitmentsandnullifiers>
fn prf_nf(a_sk: [u8; 32], rho: [u8; 32]) -> [u8; 32] {
    let mut state = [0u32; 8];
    let mut block = GenericArray::<u8, U64>::default();

    block.as_mut_slice()[0..32].copy_from_slice(&a_sk[..]);
    // The first four bits –i.e. the most signicant four bits of the
    // first byte– are used to separate distinct uses
    // of SHA256Compress, ensuring that the functions are independent.
    block.as_mut_slice()[0] |= 0b1100_0000;

    block.as_mut_slice()[32..].copy_from_slice(&rho[..]);

    sha2::compress256(&mut state, &[block]);

    let mut derived_bytes = [0u8; 32];
    LittleEndian::write_u32_into(&state, &mut derived_bytes);

    derived_bytes
}

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

impl<'a> From<(&'a SpendingKey, NullifierSeed)> for Nullifier {
    fn from((a_sk, rho): (&'a SpendingKey, NullifierSeed)) -> Self {
        Self(prf_nf(a_sk.into(), rho.into()))
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
