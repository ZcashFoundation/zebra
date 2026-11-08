//! A faster Sapling Merkle hash with the same output as `sapling_crypto`'s.
//!
//! A Merkle hash is a Pedersen hash of a fixed 516-bit input (6 level bits, then 255 bits of
//! each child), so it is a sum of one fixed point per group of 3 input chunks. This module
//! precomputes those points for every 9-bit group, hashes with 58 mixed additions, and
//! converts whole tree levels to affine coordinates with one field inversion.

use std::sync::LazyLock;

use group::{ff::PrimeField, Curve};
use incrementalmerkletree::Level;
use sapling_crypto::constants::{PEDERSEN_HASH_CHUNKS_PER_GENERATOR, PEDERSEN_HASH_GENERATORS};

use crate::parallel::batch_frontier::TreeHash;

#[cfg(test)]
mod tests;

/// 3-bit Pedersen chunks in a Merkle hash input: `(6 + 2 * 255) / 3`.
const CHUNKS: usize = 172;

/// Input bits per table group, 3 chunks.
const GROUP_BITS: usize = 9;

/// Table groups per hash; the last group has one chunk.
const GROUPS: usize = CHUNKS.div_ceil(3);

/// For each group, the sum of its chunks' Pedersen terms for each of its 512 bit patterns.
static TABLE: LazyLock<Vec<jubjub::AffineNielsPoint>> = LazyLock::new(build_table);

fn build_table() -> Vec<jubjub::AffineNielsPoint> {
    // Chunk `j` of generator `g` contributes `enc(chunk) * 2^(4j) * G_g`.
    let chunk_base = |chunk: usize| {
        let generator = chunk / PEDERSEN_HASH_CHUNKS_PER_GENERATOR;
        let index = chunk % PEDERSEN_HASH_CHUNKS_PER_GENERATOR;
        let mut point = jubjub::ExtendedPoint::from(PEDERSEN_HASH_GENERATORS[generator]);
        for _ in 0..4 * index {
            point = point.double();
        }
        point
    };

    // enc(a, b, c) = (1 + a + 2b) * (1 - 2c), indexed by the chunk bits `a | b << 1 | c << 2`.
    let chunk_terms = |base: jubjub::ExtendedPoint| {
        let multiples = [
            base,
            base.double(),
            base.double() + base,
            base.double().double(),
        ];
        let mut terms = [jubjub::ExtendedPoint::identity(); 8];
        for (bits, term) in terms.iter_mut().enumerate() {
            let multiple = multiples[(bits & 1) + 2 * ((bits >> 1) & 1)];
            *term = if bits & 4 == 0 { multiple } else { -multiple };
        }
        terms
    };

    let mut sums = Vec::with_capacity(GROUPS << GROUP_BITS);
    for group in 0..GROUPS {
        let terms: Vec<_> = (3 * group..CHUNKS.min(3 * group + 3))
            .map(|chunk| chunk_terms(chunk_base(chunk)))
            .collect();
        for pattern in 0..1usize << GROUP_BITS {
            let mut sum = jubjub::ExtendedPoint::identity();
            for (offset, terms) in terms.iter().enumerate() {
                sum += terms[(pattern >> (3 * offset)) & 7];
            }
            sums.push(sum);
        }
    }

    let mut affine = vec![jubjub::AffinePoint::identity(); sums.len()];
    jubjub::ExtendedPoint::batch_normalize(&sums, &mut affine);
    affine.iter().map(jubjub::AffinePoint::to_niels).collect()
}

/// Returns the 255 low bits of a node, as little-endian limbs.
fn node_limbs(node: &sapling_crypto::Node) -> [u64; 4] {
    let bytes = jubjub::Base::from(*node).to_repr();
    let mut limbs = [0u64; 4];
    for (limb, chunk) in limbs.iter_mut().zip(bytes.chunks_exact(8)) {
        *limb = u64::from_le_bytes(chunk.try_into().expect("8-byte chunks"));
    }
    limbs[3] &= u64::MAX >> 1;
    limbs
}

/// ORs `value` into `stream`, starting at bit `shift`.
fn or_shifted(stream: &mut [u64; 9], value: &[u64; 4], shift: usize) {
    let (word, bit) = (shift / 64, shift % 64);
    for (index, limb) in value.iter().enumerate() {
        stream[word + index] |= limb << bit;
        if bit != 0 {
            stream[word + index + 1] |= limb >> (64 - bit);
        }
    }
}

/// The Pedersen hash of a Merkle node's input, before the conversion to affine coordinates.
fn hash_point(
    level: Level,
    lhs: &sapling_crypto::Node,
    rhs: &sapling_crypto::Node,
) -> jubjub::ExtendedPoint {
    // The input bits, least significant first: the level, then the children.
    let mut stream = [0u64; 9];
    stream[0] = u64::from(u8::from(level)) & 0x3f;
    or_shifted(&mut stream, &node_limbs(lhs), 6);
    or_shifted(&mut stream, &node_limbs(rhs), 6 + 255);

    let table = &*TABLE;
    let mut point = jubjub::ExtendedPoint::identity();
    for group in 0..GROUPS {
        let position = group * GROUP_BITS;
        let (word, bit) = (position / 64, position % 64);
        let mut pattern = stream[word] >> bit;
        if bit + GROUP_BITS > 64 {
            pattern |= stream[word + 1] << (64 - bit);
        }
        let pattern = usize::try_from(pattern & 0x1ff).expect("9 bits fit in usize");
        point += &table[(group << GROUP_BITS) | pattern];
    }
    point
}

fn node_from_affine(point: &jubjub::AffinePoint) -> sapling_crypto::Node {
    sapling_crypto::Node::from_scalar(point.get_u())
}

impl TreeHash for sapling_crypto::Node {
    const BATCHED_LEVELS: bool = true;

    fn combine_one(level: Level, lhs: &Self, rhs: &Self) -> Self {
        node_from_affine(&hash_point(level, lhs, rhs).to_affine())
    }

    fn combine_level(level: Level, nodes: &[Self]) -> Vec<Self> {
        let points: Vec<_> = nodes
            .chunks_exact(2)
            .map(|pair| hash_point(level, &pair[0], &pair[1]))
            .collect();
        let mut affine = vec![jubjub::AffinePoint::identity(); points.len()];
        jubjub::ExtendedPoint::batch_normalize(&points, &mut affine);
        affine.iter().map(node_from_affine).collect()
    }
}
