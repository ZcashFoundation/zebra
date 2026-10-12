// Disabled due to warnings in criterion macros.
#![allow(missing_docs)]

use std::{hint::black_box, sync::Arc};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use halo2::pasta::pallas;
use incrementalmerkletree::Hashable;

use zebra_chain::{orchard, parallel::tree::NoteCommitmentTrees, sapling};

fn orchard_combine(c: &mut Criterion) {
    let left = orchard::tree::Node::from(pallas::Base::from(3));
    let right = orchard::tree::Node::from(pallas::Base::from(5));
    c.bench_function("orchard_combine", |b| {
        b.iter(|| {
            orchard::tree::Node::combine(black_box(15.into()), black_box(&left), black_box(&right))
        });
    });
}

fn note_commitment_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("note_commitment_tree");
    for count in [1u64, 16, 256, 4096] {
        group.throughput(Throughput::Elements(count));
        let sapling_commitments: Vec<_> = (0..count)
            .map(|value| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&value.to_le_bytes());
                sapling::tree::NoteCommitmentUpdate::from_bytes(&bytes)
                    .expect("small integers are canonical field elements")
            })
            .collect();
        let orchard_commitments: Vec<_> = (0..count).map(pallas::Base::from).collect();

        // Both paths include the final root calculation. The public per-pool block-update
        // methods exist before batching, so these benchmarks also run on the sequential version.
        group.bench_with_input(
            BenchmarkId::new("sapling_append", count),
            &sapling_commitments,
            |b, commitments| {
                b.iter_batched(
                    sapling::tree::NoteCommitmentTree::default,
                    |mut tree| {
                        for commitment in black_box(commitments) {
                            tree.append(*commitment)
                                .expect("the batch fits in the tree");
                        }
                        black_box(tree.root());
                        tree
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("sapling_block_update", count),
            &sapling_commitments,
            |b, commitments| {
                b.iter_batched(
                    || {
                        (
                            Arc::new(sapling::tree::NoteCommitmentTree::default()),
                            commitments.clone(),
                        )
                    },
                    |(tree, commitments)| {
                        NoteCommitmentTrees::update_sapling_note_commitment_tree(
                            black_box(tree),
                            black_box(commitments),
                        )
                        .expect("the batch fits in the tree")
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("orchard_append", count),
            &orchard_commitments,
            |b, commitments| {
                b.iter_batched(
                    orchard::tree::NoteCommitmentTree::default,
                    |mut tree| {
                        for commitment in black_box(commitments) {
                            tree.append(*commitment)
                                .expect("the batch fits in the tree");
                        }
                        black_box(tree.root());
                        tree
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("orchard_block_update", count),
            &orchard_commitments,
            |b, commitments| {
                b.iter_batched(
                    || {
                        (
                            Arc::new(orchard::tree::NoteCommitmentTree::default()),
                            commitments.clone(),
                        )
                    },
                    |(tree, commitments)| {
                        NoteCommitmentTrees::update_orchard_note_commitment_tree(
                            black_box(tree),
                            black_box(commitments),
                        )
                        .expect("the batch fits in the tree")
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.05).sample_size(50);
    targets = orchard_combine, note_commitment_tree
);
criterion_main!(benches);
