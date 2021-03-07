#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use super::super::{
    commitment::{pedersen_hashes::mixing_pedersen_hash, NoteCommitment},
    keys::NullifierDerivingKey,
    tree::Position,
};

/// Invokes Blake2s-256 as PRF^nfSapling to derive the nullifier for a
/// Sapling note.
///
/// PRF^nfSapling(ρ*) := BLAKE2s-256("Zcash_nf", nk* || ρ*)
///
/// https://zips.z.cash/protocol/protocol.pdf#concreteprfs
fn prf_nf(nk: [u8; 32], rho: [u8; 32]) -> [u8; 32] {
    let hash = blake2s_simd::Params::new()
        .hash_length(32)
        .personal(b"Zcash_nf")
        .to_state()
        .update(&nk[..])
        .update(&rho[..])
        .finalize();

    *hash.as_array()
}

/// A Nullifier for Sapling transactions
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct Nullifier(pub [u8; 32]);

impl From<[u8; 32]> for Nullifier {
    fn from(buf: [u8; 32]) -> Self {
        Self(buf)
    }
}

impl<'a> From<(NoteCommitment, Position, &'a NullifierDerivingKey)> for Nullifier {
    fn from((cm, pos, nk): (NoteCommitment, Position, &'a NullifierDerivingKey)) -> Self {
        let rho = jubjub::AffinePoint::from(mixing_pedersen_hash(cm.0.into(), pos.0.into()));

        Nullifier(prf_nf(nk.into(), rho.to_bytes()))
    }
}

impl From<Nullifier> for [u8; 32] {
    fn from(n: Nullifier) -> Self {
        n.0
    }
}
