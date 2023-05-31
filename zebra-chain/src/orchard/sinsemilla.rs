//! Sinsemilla hash functions and helpers.

use bitvec::prelude::*;
use halo2::{
    arithmetic::{Coordinates, CurveAffine, CurveExt},
    pasta::{group::Group, pallas},
};

/// [Coordinate Extractor for Pallas][concreteextractorpallas]
///
/// ExtractP: P → P𝑥 such that ExtractP(𝑃) = 𝑥(𝑃) mod 𝑞P.
///
/// ExtractP returns the type P𝑥 which is precise for its range, unlike
/// ExtractJ(𝑟) which returns a bit sequence.
///
/// [concreteextractorpallas]: https://zips.z.cash/protocol/nu5.pdf#concreteextractorpallas
pub fn extract_p(point: pallas::Point) -> pallas::Base {
    let option: Option<Coordinates<pallas::Affine>> =
        pallas::Affine::from(point).coordinates().into();

    match option {
        // If Some, it's not the identity.
        Some(coordinates) => *coordinates.x(),
        _ => pallas::Base::zero(),
    }
}

/// Extract⊥ P: P ∪ {⊥} → P𝑥 ∪ {⊥} such that
///
///  Extract⊥ P(︀⊥)︀ = ⊥
///  Extract⊥ P(︀𝑃: P)︀ = ExtractP(𝑃).
///
/// <https://zips.z.cash/protocol/nu5.pdf#concreteextractorpallas>
pub fn extract_p_bottom(maybe_point: Option<pallas::Point>) -> Option<pallas::Base> {
    // Maps an Option<T> to Option<U> by applying a function to a contained value.
    maybe_point.map(extract_p)
}

/// GroupHash into Pallas, aka _GroupHash^P_
///
/// Produces a random point in the Pallas curve.  The first input element acts
/// as a domain separator to distinguish uses of the group hash for different
/// purposes; the second input element is the message.
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretegrouphashpallasandvesta>
#[allow(non_snake_case)]
pub fn pallas_group_hash(D: &[u8], M: &[u8]) -> pallas::Point {
    let domain_separator = std::str::from_utf8(D).unwrap();

    pallas::Point::hash_to_curve(domain_separator)(M)
}

/// Q(D) := GroupHash^P(︀“z.cash:SinsemillaQ”, D)
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash>
#[allow(non_snake_case)]
fn Q(D: &[u8]) -> pallas::Point {
    pallas_group_hash(b"z.cash:SinsemillaQ", D)
}

/// S(j) := GroupHash^P(︀“z.cash:SinsemillaS”, LEBS2OSP32(I2LEBSP32(j)))
///
/// S: {0 .. 2^k - 1} -> P^*, aka 10 bits hashed into the group
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash>
#[allow(non_snake_case)]
fn S(j: &BitSlice<u8, Lsb0>) -> pallas::Point {
    // The value of j is a 10-bit value, therefore must never exceed 2^10 in
    // value.
    assert_eq!(j.len(), 10);

    // I2LEOSP_32(𝑗)
    let mut leosp_32_j = [0u8; 4];
    leosp_32_j[..2].copy_from_slice(j.to_bitvec().as_raw_slice());

    pallas_group_hash(b"z.cash:SinsemillaS", &leosp_32_j)
}

/// Incomplete addition on the Pallas curve.
///
/// P ∪ {⊥} × P ∪ {⊥} → P ∪ {⊥}
///
/// <https://zips.z.cash/protocol/protocol.pdf#concretesinsemillahash>
fn incomplete_addition(
    left: Option<pallas::Point>,
    right: Option<pallas::Point>,
) -> Option<pallas::Point> {
    let identity = pallas::Point::identity();

    match (left, right) {
        (None, _) | (_, None) => None,
        (Some(l), _) if l == identity => None,
        (_, Some(r)) if r == identity => None,
        (Some(l), Some(r)) if l == r => None,
        // The inverse of l, (x, -y)
        (Some(l), Some(r)) if l == -r => None,
        (Some(l), Some(r)) => Some(l + r),
    }
}

/// "...an algebraic hash function with collision resistance (for fixed input
/// length) derived from assumed hardness of the Discrete Logarithm Problem on
/// the Pallas curve."
///
/// SinsemillaHash is used in the definitions of Sinsemilla commitments and of
/// the Sinsemilla hash for the Orchard incremental Merkle tree (§ 5.4.1.3
/// ‘MerkleCRH^Orchard Hash Function’).
///
/// SinsemillaHashToPoint(𝐷: B^Y^\[N\] , 𝑀 : B ^[{0 .. 𝑘·𝑐}] ) → P ∪ {⊥}
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash>
///
/// # Panics
///
/// If `M` is greater than `k*c = 2530` bits.
#[allow(non_snake_case)]
pub fn sinsemilla_hash_to_point(D: &[u8], M: &BitVec<u8, Lsb0>) -> Option<pallas::Point> {
    let k = 10;
    let c = 253;

    assert!(M.len() <= k * c);

    let mut acc = Some(Q(D));

    // Split M into n segments of k bits, where k = 10 and c = 253, padding
    // the last segment with zeros.
    //
    // https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash
    for chunk in M.chunks(k) {
        // Pad each chunk with zeros.
        let mut store = [0u8; 2];
        let bits = BitSlice::<_, Lsb0>::from_slice_mut(&mut store);
        bits[..chunk.len()].copy_from_bitslice(chunk);

        acc = incomplete_addition(incomplete_addition(acc, Some(S(&bits[..k]))), acc);
    }

    acc
}

/// Sinsemilla Hash Function
///
/// "SinsemillaHash is an algebraic hash function with collision resistance (for
/// fixed input length) derived from assumed hardness of the Discrete Logarithm
/// Problem. It is designed by Sean Bowe and Daira Hopwood. The motivation for
/// introducing a new discrete-log-based hash function (rather than using
/// PedersenHash) is to make efcient use of the lookups available in recent
/// proof systems including Halo 2."
///
/// SinsemillaHash: B^Y^\[N\] × B[{0 .. 𝑘·𝑐}] → P_𝑥 ∪ {⊥}
///
/// <https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash>
///
/// # Panics
///
/// If `M` is greater than `k*c = 2530` bits in `sinsemilla_hash_to_point`.
#[allow(non_snake_case)]
pub fn sinsemilla_hash(D: &[u8], M: &BitVec<u8, Lsb0>) -> Option<pallas::Base> {
    extract_p_bottom(sinsemilla_hash_to_point(D, M))
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::orchard::tests::vectors;

    #[cfg(test)]
    fn x_from_str(s: &str) -> pallas::Base {
        use halo2::pasta::group::ff::PrimeField;

        pallas::Base::from_str_vartime(s).unwrap()
    }

    #[test]
    #[allow(non_snake_case)]
    fn single_test_vector() {
        use halo2::pasta::group::Curve;

        let D = b"z.cash:test-Sinsemilla";
        let M = bitvec![
            u8, Lsb0; 0, 0, 0, 1, 0, 1, 1, 0, 1, 0, 1, 0, 0, 1, 1, 0, 0, 0, 1, 1, 0, 1, 1, 0, 0, 0,
            1, 1, 0, 1, 1, 0, 1, 1, 1, 1, 0, 1, 1, 0,
        ];

        let test_vector = pallas::Affine::from_xy(
            x_from_str(
                "19681977528872088480295086998934490146368213853811658798708435106473481753752",
            ),
            x_from_str(
                "14670850419772526047574141291705097968771694788047376346841674072293161339903",
            ),
        )
        .unwrap();

        assert_eq!(
            sinsemilla_hash_to_point(&D[..], &M).expect("").to_affine(),
            test_vector
        )
    }

    // Checks Sinsemilla hashes to point and to bytes (aka the x-coordinate
    // bytes of a point) with:
    // - One of two domains.
    // - Random message lengths between 0 and 255 bytes.
    // - Random message bits.
    #[test]
    #[allow(non_snake_case)]
    fn hackworks_test_vectors() {
        use halo2::pasta::group::{ff::PrimeField, GroupEncoding};

        for tv in tests::vectors::SINSEMILLA.iter() {
            let D = tv.domain.as_slice();
            let M: &BitVec<u8, Lsb0> = &tv.msg.iter().collect();

            assert_eq!(
                sinsemilla_hash_to_point(D, M).expect("should not fail per Theorem 5.4.4"),
                pallas::Point::from_bytes(&tv.point).unwrap()
            );

            assert_eq!(
                sinsemilla_hash(D, M).expect("should not fail per Theorem 5.4.4"),
                pallas::Base::from_repr(tv.hash).unwrap()
            )
        }
    }

    // Checks Pallas group hashes with:
    // - One of two domains.
    // - Random message lengths between 0 and 255 bytes.
    // - Random message contents.
    #[test]
    #[allow(non_snake_case)]
    fn hackworks_group_hash_test_vectors() {
        use halo2::pasta::group::GroupEncoding;

        for tv in tests::vectors::GROUP_HASHES.iter() {
            let D = tv.domain.as_slice();
            let M = tv.msg.as_slice();

            assert_eq!(
                pallas_group_hash(D, M),
                pallas::Point::from_bytes(&tv.point).unwrap()
            );
        }
    }
}
