//! Sinsemilla hash functions and helpers.

use bitvec::prelude::*;

use halo2::{
    arithmetic::{Coordinates, CurveAffine, CurveExt},
    pasta::pallas,
};

/// [Hash Extractor for Pallas][concreteextractorpallas]
///
/// P → B^[l^Orchard_Merkle]
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

/// GroupHash into Pallas, aka _GroupHash^P_
///
/// Produces a random point in the Pallas curve.  The first input element acts
/// as a domain separator to distinguish uses of the group hash for different
/// purposes; the second input element is the message.
///
/// https://zips.z.cash/protocol/nu5.pdf#concretegrouphashpallasandvesta
#[allow(non_snake_case)]
pub fn pallas_group_hash(D: &[u8], M: &[u8]) -> pallas::Point {
    let domain_separator = std::str::from_utf8(D).unwrap();

    pallas::Point::hash_to_curve(domain_separator)(M)
}

/// Q(D) := GroupHash^P(︀“z.cash:SinsemillaQ”, D)
///
/// https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash
#[allow(non_snake_case)]
fn Q(D: &[u8]) -> pallas::Point {
    pallas_group_hash(b"z.cash:SinsemillaQ", D)
}

/// S(j) := GroupHash^P(︀“z.cash:SinsemillaS”, LEBS2OSP32(I2LEBSP32(j)))
///
/// S: {0 .. 2^k - 1} -> P^*, aka 10 bits hashed into the group
///
/// https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash
#[allow(non_snake_case)]
fn S(j: &BitSlice<Lsb0, u8>) -> pallas::Point {
    // The value of j is a 10-bit value, therefore must never exceed 2^10 in
    // value.
    assert_eq!(j.len(), 10);

    pallas_group_hash(b"z.cash:SinsemillaS", j.as_slice())
}

/// "...an algebraic hash function with collision resistance (for fixed input
/// length) derived from assumed hardness of the Discrete Logarithm Problem on
/// the Pallas curve."
///
/// SinsemillaHash is used in the definitions of Sinsemilla commitments and of
/// the Sinsemilla hash for the Orchard incremental Merkle tree (§ 5.4.1.3
/// ‘MerkleCRH^Orchard Hash Function’).
///
/// https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash
///
/// # Panics
///
/// If `M` is greater than `k*c = 2530` bits.
#[allow(non_snake_case)]
pub fn sinsemilla_hash_to_point(D: &[u8], M: &BitVec<Lsb0, u8>) -> pallas::Point {
    let k = 10;
    let c = 253;

    assert!(M.len() <= k * c);

    let mut acc = Q(D);

    // Split M into n segments of k bits, where k = 10 and c = 253, padding
    // the last segment with zeros.
    //
    // https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash
    for chunk in M.chunks(k) {
        // Pad each chunk with zeros.
        let mut store = [0u8; 2];
        let bits = store.bits_mut::<Lsb0>();
        bits[..chunk.len()].copy_from_slice(chunk);

        acc = acc + acc + S(&bits[..k]);
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
/// https://zips.z.cash/protocol/nu5.pdf#concretesinsemillahash
///
/// # Panics
///
/// If `M` is greater than `k*c = 2530` bits in `sinsemilla_hash_to_point`.
#[allow(non_snake_case)]
pub fn sinsemilla_hash(D: &[u8], M: &BitVec<Lsb0, u8>) -> pallas::Base {
    extract_p(sinsemilla_hash_to_point(D, M))
}

/// Sinsemilla commit
///
/// We construct Sinsemilla commitments by hashing to a point with Sinsemilla
/// hash, and adding a randomized point on the Pallas curve.
///
/// SinsemillaCommit_r(D, M) := SinsemillaHashToPoint(D || "-M", M) + [r]GroupHash^P(D || "-r", "")
///
/// https://zips.z.cash/protocol/nu5.pdf#concretesinsemillacommit
#[allow(non_snake_case)]
pub fn sinsemilla_commit(r: pallas::Scalar, D: &[u8], M: &BitVec<Lsb0, u8>) -> pallas::Point {
    sinsemilla_hash_to_point(&[D, b"-M"].concat(), M)
        + pallas_group_hash(&[D, b"-r"].concat(), b"") * r
}

/// SinsemillaShortCommit_r(D, M) := Extract_P(SinsemillaCommit_r(D, M))
///
/// https://zips.z.cash/protocol/nu5.pdf#concretesinsemillacommit
#[allow(non_snake_case)]
pub fn sinsemilla_short_commit(r: pallas::Scalar, D: &[u8], M: &BitVec<Lsb0, u8>) -> pallas::Base {
    extract_p(sinsemilla_commit(r, D, M))
}

// TODO: test the above correctness and compatibility with the zcash-hackworks test vectors
// https://github.com/ZcashFoundation/zebra/issues/2079
// https://github.com/zcash-hackworks/zcash-test-vectors/pulls
