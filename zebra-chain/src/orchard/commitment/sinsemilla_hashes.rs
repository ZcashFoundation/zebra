//! Sinsemilla hash functions and helpers.

use bitvec::prelude::*;
use rand_core::{CryptoRng, RngCore};

use halo2::pasta::pallas;

/// [Hash Extractor for Pallas][concreteextractorpallas]
///
/// P → B^[l^Orchard_Merkle]
///
/// [concreteextractorpallas]: https://zips.z.cash/protocol/nu5.pdf#concreteextractorpallas
// TODO: should this return the basefield element type, or the bytes?
pub fn extract_p(point: pallas::Point) -> pallas::Base {
    match pallas::Affine::from(point).get_xy().into() {
        // If Some, it's not the identity.
        Some((x, _)) => x,
        _ => pallas::Base::zero(),
    }
}

/// Q
///
/// Q(D) := GroupHash^P(︀“z.cash:SinsemillaQ”, D)
///
/// https://zips.z.cash/protocol/protocol.pdf#concretesinsemillahash
#[allow(non_snake_case)]
fn Q(domain: [u8; 8]) -> pallas::Point {
    pallas::Point::hash_to_curve("z.cash:SinsemillaQ")(&domain[..])
}

/// S
///
/// S(j) := GroupHash^P(︀“z.cash:SinsemillaS”, LEBS2OSP32(I2LEBSP32(j)))
///
/// https://zips.z.cash/protocol/protocol.pdf#concretesinsemillahash
#[allow(non_snake_case)]
fn S(j: [u8; 2]) -> pallas::Point {
    pallas::Point::hash_to_curve("z.cash:SinsemillaS")(&j)
}

/// "...an algebraic hash function with collision resistance (for fixed input
/// length) derived from assumed hardness of the Discrete Logarithm Problem on
/// the Jubjub curve."
///
/// SinsemillaHash is used in the definitions of Sinsemilla commitments and of
/// the Sinsemilla hash for the Sapling incremental Merkle tree (§ 5.4.1.3
/// ‘MerkleCRH^Orchard Hash Function’).
///
/// https://zips.z.cash/protocol/protocol.pdf#concretesinsemillahash
#[allow(non_snake_case)]
pub fn sinsemilla_hash_to_point(domain: [u8; 8], M: &BitVec<Lsb0, u8>) -> pallas::Point {
    const K: u8 = 10;
    const C: u8 = 253;

    assert!(M.len() <= K * C);

    let mut acc = Q(domain);

    // Split M into n segments of k bits, where k = 10 and c = 253, padding
    // the last segment with zeros.
    //
    // https://zips.z.cash/protocol/protocol.pdf#concretesinsemillahash
    for chunk in M.chunks(K) {
        // Pad each chunk with zeros.
        let mut store = 0u8;
        let bits = store.bits_mut::<Lsb0>();
        chunk
            .iter()
            .enumerate()
            .for_each(|(i, bit)| bits.set(i, *bit));

        // An instance of LEBS2IP_k
        let j = &bits.iter().fold(0u16, |j, &bit| j * *2 + bit as u16);

        acc += (acc + S(j.to_le_bytes()));
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
/// https://zips.z.cash/protocol/protocol.pdf#concretesinsemillahash
#[allow(non_snake_case)]
pub fn sinsemilla_hash(domain: [u8; 8], M: &BitVec<Lsb0, u8>) -> pallas::Base {
    extract_p(sinsemilla_hash_to_point(domain, M))
}

/// Generates a random scalar from the scalar field 𝔽_{q_P}.
///
/// https://zips.z.cash/protocol/nu5.pdf#pallasandvesta
pub fn generate_trapdoor<T>(csprng: &mut T) -> pallas::Scalar
where
    T: RngCore + CryptoRng,
{
    let mut bytes = [0u8; 64];
    csprng.fill_bytes(&mut bytes);
    // Scalar::from_bytes_wide() reduces the input modulo q under the hood.
    pallas::Scalar::from_bytes_wide(&bytes)
}
