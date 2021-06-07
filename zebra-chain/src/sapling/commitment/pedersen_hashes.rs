//! Pedersen hash functions and helpers.

use bitvec::prelude::*;

use super::super::keys::find_group_hash;

/// I_i
///
/// Expects i to be 1-indexed from the loop it's called in.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
#[allow(non_snake_case)]
fn I_i(domain: [u8; 8], i: u32) -> jubjub::ExtendedPoint {
    find_group_hash(domain, &(i - 1).to_le_bytes())
}

/// The encoding function ⟨Mᵢ⟩
///
/// Σ j={0,k-1}: (1 - 2x₂)⋅(1 + x₀ + 2x₁)⋅2^(4⋅j)
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
#[allow(non_snake_case)]
fn M_i(segment: &BitSlice<Lsb0, u8>) -> jubjub::Fr {
    let mut m_i = jubjub::Fr::zero();

    for (j, chunk) in segment.chunks(3).enumerate() {
        // Pad each chunk with zeros.
        let mut store = 0u8;
        let bits = store.bits_mut::<Lsb0>();
        chunk
            .iter()
            .enumerate()
            .for_each(|(i, bit)| bits.set(i, *bit));

        let mut tmp = jubjub::Fr::one();

        if bits[0] {
            tmp += &jubjub::Fr::one();
        }

        if bits[1] {
            tmp += &jubjub::Fr::one().double();
        }

        if bits[2] {
            tmp -= tmp.double();
        }

        if j > 0 {
            // Inclusive range!
            tmp *= (1..=(4 * j)).fold(jubjub::Fr::one(), |acc, _| acc.double());
        }

        m_i += tmp;
    }

    m_i
}

/// "...an algebraic hash function with collision resistance (for fixed input
/// length) derived from assumed hardness of the Discrete Logarithm Problem on
/// the Jubjub curve."
///
/// PedersenHash is used in the definitions of Pedersen commitments (§
/// 5.4.7.2 'Windowed Pedersen commitments'), and of the Pedersen hash for the
/// Sapling incremental Merkle tree (§ 5.4.1.3 'MerkleCRH^Sapling Hash
/// Function').
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
#[allow(non_snake_case)]
pub fn pedersen_hash_to_point(domain: [u8; 8], M: &BitVec<Lsb0, u8>) -> jubjub::ExtendedPoint {
    let mut result = jubjub::ExtendedPoint::identity();

    // Split M into n segments of 3 * c bits, where c = 63, padding the last
    // segment with zeros.
    //
    // This loop is 1-indexed per the math definitions in the spec.
    //
    // https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
    for (i, segment) in M
        .chunks(189)
        .enumerate()
        .map(|(i, segment)| (i + 1, segment))
    {
        result += I_i(domain, i as u32) * M_i(segment);
    }

    result
}

/// Pedersen Hash Function
///
/// This is technically returning 255 (l_MerkleSapling) bits, not 256.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretepedersenhash
#[allow(non_snake_case)]
pub fn pedersen_hash(domain: [u8; 8], M: &BitVec<Lsb0, u8>) -> jubjub::Fq {
    jubjub::AffinePoint::from(pedersen_hash_to_point(domain, M)).get_u()
}

/// Mixing Pedersen Hash Function
///
/// Used to compute ρ from a note commitment and its position in the note
/// commitment tree.  It takes as input a Pedersen commitment P, and hashes it
/// with another input x.
///
/// MixingPedersenHash(P, x) := P + [x]FindGroupHash^J^(r)("Zcash_J_", "")
///
/// https://zips.z.cash/protocol/protocol.pdf#concretemixinghash
#[allow(non_snake_case)]
pub fn mixing_pedersen_hash(P: jubjub::ExtendedPoint, x: jubjub::Fr) -> jubjub::ExtendedPoint {
    const J: [u8; 8] = *b"Zcash_J_";

    P + find_group_hash(J, b"") * x
}

/// Construct a 'windowed' Pedersen commitment by reusing a Pederson hash
/// construction, and adding a randomized point on the Jubjub curve.
///
/// WindowedPedersenCommit_r (s) := \
///   PedersenHashToPoint("Zcash_PH", s) + [r]FindGroupHash^J^(r)("Zcash_PH", "r")
///
/// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
pub fn windowed_pedersen_commitment(r: jubjub::Fr, s: &BitVec<Lsb0, u8>) -> jubjub::ExtendedPoint {
    const D: [u8; 8] = *b"Zcash_PH";

    pedersen_hash_to_point(D, s) + find_group_hash(D, b"r") * r
}
