//!
#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use std::io;

use byteorder::{ByteOrder, LittleEndian};
use serde::{Deserialize, Serialize};

use crate::{
    keys::sprout::SpendingKey,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// PRF^nf is used to derive a Sprout nullifer from the receiver's
/// spending key a_sk and a nullifier seed ρ, instantiated using the
/// SHA-256 compression function.
///
/// https://zips.z.cash/protocol/protocol.pdf#abstractprfs
/// https://zips.z.cash/protocol/protocol.pdf#commitmentsandnullifiers
fn prf_nf(a_sk: [u8; 32], rho: [u8; 32]) -> [u8; 32] {
    let mut state = [0u32; 8];
    let mut block = [0u8; 64];

    block[0..32].copy_from_slice(&a_sk[..]);
    // The first four bits –i.e. the most signicant four bits of the
    // first byte– are used to separate distinct uses
    // ofSHA256Compress, ensuring that the functions are independent.
    block[0] |= 0b1110_0000;

    block[32..].copy_from_slice(&rho[..]);

    sha2::compress256(&mut state, &block);

    let mut derived_bytes = [0u8; 32];
    LittleEndian::write_u32_into(&state, &mut derived_bytes);

    derived_bytes
}

/// Nullifier seed, named rho in the [spec][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents
#[derive(Clone, Copy, Debug)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct NullifierSeed([u8; 32]);

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

/// A Nullifier for Sprout transactions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Nullifier([u8; 32]);

impl From<NullifierSeed> for [u8; 32] {
    fn from(rho: NullifierSeed) -> Self {
        rho.0
    }
}

impl From<(SpendingKey, NullifierSeed)> for Nullifier {
    fn from((a_sk, rho): (SpendingKey, NullifierSeed)) -> Self {
        Self(prf_nf(a_sk.into(), rho.into()))
    }
}

impl ZcashDeserialize for Nullifier {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let bytes = reader.read_32_bytes()?;

        Ok(Self(bytes))
    }
}

impl ZcashSerialize for Nullifier {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])
    }
}
