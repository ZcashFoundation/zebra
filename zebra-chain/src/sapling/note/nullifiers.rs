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
/// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
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

impl From<Nullifier> for [jubjub::Fq; 2] {
    /// Add the nullifier through multiscalar packing
    ///
    /// Informed by <https://github.com/zkcrypto/bellman/blob/main/src/gadgets/multipack.rs>
    fn from(n: Nullifier) -> Self {
        use std::ops::AddAssign;

        let nullifier_bits_le: Vec<bool> =
            n.0.iter()
                .flat_map(|&v| (0..8).map(move |i| (v >> i) & 1 == 1))
                .collect();

        // The number of bits needed to represent the modulus, minus 1.
        const CAPACITY: usize = 255 - 1;

        let mut result = [jubjub::Fq::zero(); 2];

        // Since we know the max bits of the input (256) and the chunk size
        // (254), this will always result in 2 chunks.
        for (i, bits) in nullifier_bits_le.chunks(CAPACITY).enumerate() {
            let mut cur = jubjub::Fq::zero();
            let mut coeff = jubjub::Fq::one();

            for bit in bits {
                if *bit {
                    cur.add_assign(&coeff);
                }

                coeff = coeff.double();
            }

            result[i] = cur
        }

        result
    }
}
