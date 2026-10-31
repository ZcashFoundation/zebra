//! Sinsemilla hash functions and helpers.

use bitvec::prelude::*;
use halo2::pasta::pallas;
use sinsemilla::HashDomain;

/// Sinsemilla Hash Function
///
/// "SinsemillaHash is an algebraic hash function with collision resistance (for
/// fixed input length) derived from assumed hardness of the Discrete Logarithm
/// Problem. It is designed by Sean Bowe and Daira Hopwood. The motivation for
/// introducing a new discrete-log-based hash function (rather than using
/// PedersenHash) is to make efficient use of the lookups available in recent
/// proof systems including Halo 2."
///
/// SinsemillaHash: B^Y^\[N\] × B[{0 .. 𝑘·𝑐}] → P_𝑥 ∪ {⊥}
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash>
#[allow(non_snake_case)]
pub fn sinsemilla_hash(D: &[u8], M: &BitVec<u8, Lsb0>) -> Option<pallas::Base> {
    let domain = std::str::from_utf8(D).expect("must be valid UTF-8");
    let hash_domain = HashDomain::new(domain);

    hash_domain.hash(M.iter().map(|b| *b.as_ref())).into()
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::orchard::tests::vectors;

    // Checks Sinsemilla hashes to point and to bytes (aka the x-coordinate
    // bytes of a point) with:
    // - One of two domains.
    // - Random message lengths between 0 and 255 bytes.
    // - Random message bits.
    #[test]
    #[allow(non_snake_case)]
    fn sinsemilla_hackworks_test_vectors() {
        use halo2::pasta::group::ff::PrimeField;

        for tv in tests::vectors::SINSEMILLA.iter() {
            let D = tv.domain.as_slice();
            let M: &BitVec<u8, Lsb0> = &tv.msg.iter().collect();

            assert_eq!(
                sinsemilla_hash(D, M).expect("should not fail per Theorem 5.4.4"),
                pallas::Base::from_repr(tv.hash).unwrap()
            )
        }
    }
}
