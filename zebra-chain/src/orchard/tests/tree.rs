use bitvec::prelude::*;
use halo2::pasta::{group::ff::PrimeField, pallas};

use crate::orchard::tests::vectors;
use crate::orchard::tree::*;

#[test]
fn empty_roots() {
    let _init_guard = zebra_test::init();

    for i in 0..EMPTY_ROOTS.len() {
        assert_eq!(
            EMPTY_ROOTS[i].to_repr(),
            // The test vector is in reversed order.
            vectors::EMPTY_ROOTS[usize::from(MERKLE_DEPTH) - i]
        );
    }
}

#[test]
fn incremental_roots() {
    let _init_guard = zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = NoteCommitmentTree::default();

    for (i, commitment_set) in vectors::COMMITMENTS.iter().enumerate() {
        for cm_x_bytes in commitment_set.iter() {
            let cm_x = pallas::Base::from_repr(*cm_x_bytes).unwrap();

            leaves.push(cm_x);

            let _ = incremental_tree.append(cm_x);
        }

        assert_eq!(
            hex::encode(incremental_tree.hash()),
            hex::encode(vectors::ROOTS[i].anchor)
        );

        assert_eq!(
            hex::encode((NoteCommitmentTree::from(leaves.clone())).hash()),
            hex::encode(vectors::ROOTS[i].anchor)
        );
    }
}

/// The independent reference builds the message bit-by-bit and retains the
/// exceptional-case behavior of the variable-length Sinsemilla implementation.
fn merkle_crh_orchard_uncached(layer: u8, left: pallas::Base, right: pallas::Base) -> pallas::Base {
    let mut message = bitvec![u8, Lsb0;];
    let level = MERKLE_DEPTH - 1 - layer;
    message.extend_from_bitslice(&BitArray::<_, Lsb0>::from([level, 0])[..10]);
    message.extend_from_bitslice(&BitArray::<_, Lsb0>::from(left.to_repr())[..255]);
    message.extend_from_bitslice(&BitArray::<_, Lsb0>::from(right.to_repr())[..255]);

    crate::orchard::sinsemilla::sinsemilla_hash(b"z.cash:Orchard-MerkleCRH", &message)
        .unwrap_or(pallas::Base::zero())
}

/// Field elements spanning the full child encoding, including the largest
/// canonical values and raw limbs reduced modulo the field modulus.
fn full_width_field_elements() -> Vec<pallas::Base> {
    vec![
        pallas::Base::zero() - pallas::Base::one(),
        pallas::Base::zero() - pallas::Base::from(2),
        pallas::Base::from_raw([u64::MAX, u64::MAX, u64::MAX, u64::MAX]),
        pallas::Base::from_raw([0, 0, 0, u64::MAX]),
        pallas::Base::from_raw([
            0x0123_4567_89ab_cdef,
            0xfedc_ba98_7654_3210,
            0xdead_beef_cafe_babe,
            0x0f1e_2d3c_4b5a_6978,
        ]),
    ]
}

/// Differential checks on ordinary inputs cover all layers, child-word
/// alignments, and the cross-child word. They do not establish equivalence
/// on the exceptional inputs discussed in the weighted evaluator's reduction.
#[test]
fn weighted_merkle_crh_matches_fresh_domain_all_layers() {
    let mut values: Vec<pallas::Base> = [0u64, 1, 2, 7, 65_535, u64::MAX]
        .into_iter()
        .map(pallas::Base::from)
        .collect();
    values.extend(full_width_field_elements());

    for layer in 0..MERKLE_DEPTH {
        for &left in &values {
            for &right in &values {
                assert_eq!(
                    merkle_crh_orchard(layer, left, right).to_repr(),
                    merkle_crh_orchard_uncached(layer, left, right).to_repr(),
                    "weighted evaluator must match fresh domain at layer {layer}",
                );
            }
        }
    }
}

proptest::proptest! {
    /// Random ordinary inputs check full-width child packing and weighted hashing.
    #[test]
    fn weighted_merkle_crh_matches_fresh_domain_random(
        layer in 0u8..MERKLE_DEPTH,
        left_limbs in proptest::prelude::any::<[u64; 4]>(),
        right_limbs in proptest::prelude::any::<[u64; 4]>(),
    ) {
        let left = pallas::Base::from_raw(left_limbs);
        let right = pallas::Base::from_raw(right_limbs);

        proptest::prop_assert_eq!(
            merkle_crh_orchard(layer, left, right).to_repr(),
            merkle_crh_orchard_uncached(layer, left, right).to_repr(),
            "weighted evaluator must match fresh domain at layer {}", layer
        );
    }
}
