//! Benchmarks for Groth16 proof verification (Sprout JoinSplits).
//!
//! These benchmarks measure the cost of verifying Groth16 zero-knowledge proofs
//! used in Sprout JoinSplit descriptions. Groth16 verification involves a pairing
//! check on BLS12-381, making it one of the most expensive per-proof operations
//! during chain sync.
//!
//! # Test data limitations
//!
//! The hardcoded mainnet test blocks in `zebra-test` contain only a small number
//! of Groth16 JoinSplits (~5 items). To benchmark realistic batch sizes, items
//! are cycled (repeated) to fill larger batches.
//!
//! This is valid for measuring verification throughput because:
//! - Groth16 pairing checks perform the same curve operations regardless of the
//!   specific proof bytes — the cost is constant per proof.
//! - Primary input preparation (blake2b hashing + field element packing) is also
//!   constant-cost regardless of input values.
//!
//! However, this means the benchmarks do **not** capture any cache effects or
//! memory access patterns that might differ with truly unique proofs at scale.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use bellman::groth16::PreparedVerifyingKey;
use bls12_381::Bls12;

use zebra_chain::{
    block::Block, primitives::Groth16Proof, serialization::ZcashDeserializeInto, sprout::JoinSplit,
};

use zebra_consensus::groth16::{Description, DescriptionWrapper, Item, SPROUT};

/// Extracts valid Groth16 items from the hardcoded mainnet test blocks.
///
/// Iterates over `MAINNET_BLOCKS`, deserializes each block, and collects all
/// Groth16 JoinSplit proofs along with their primary inputs. Only V4 transactions
/// contain Groth16 JoinSplits (earlier versions use BCTV14 proofs, V5+ dropped
/// Sprout support).
///
/// Returns a small set of real, valid proof items (~5 from current test vectors).
fn extract_groth16_items_from_blocks() -> Vec<Item> {
    let mut items = Vec::new();

    for (_height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes.zcash_deserialize_into().expect("valid block");

        for tx in &block.transactions {
            let joinsplits: Vec<&JoinSplit<Groth16Proof>> =
                tx.sprout_groth16_joinsplits().collect();

            if joinsplits.is_empty() {
                continue;
            }

            let pub_key = tx
                .sprout_joinsplit_pub_key()
                .expect("pub key must exist since there are joinsplits");

            for joinsplit in joinsplits {
                let item: Item = DescriptionWrapper(&(joinsplit, &pub_key))
                    .try_into()
                    .expect("valid groth16 item");
                items.push(item);
            }
        }
    }

    assert!(
        !items.is_empty(),
        "test blocks must contain Groth16 JoinSplits"
    );
    items
}

/// Creates a batch of `n` items by cycling through the source items.
///
/// Since Groth16 verification cost is constant per proof (the same pairing
/// operations run regardless of proof content), repeating proofs produces
/// benchmarks with the same per-item cost as unique proofs would.
fn cycled_items(source: &[Item], n: usize) -> Vec<Item> {
    source.iter().cycle().take(n).cloned().collect()
}

/// Benchmarks Groth16 proof verification at various batch sizes, and the cost
/// of preparing primary inputs from JoinSplit descriptions.
fn bench_groth16_verify(c: &mut Criterion) {
    let pvk: &'static PreparedVerifyingKey<Bls12> = SPROUT.prepared_verifying_key();
    let source_items = extract_groth16_items_from_blocks();

    // -- Verification benchmarks --

    let mut group = c.benchmark_group("Groth16 Verification");

    // Single proof verification: baseline cost of one pairing check.
    group.throughput(Throughput::Elements(1));
    group.bench_function("single proof", |b| {
        let item = source_items[0].clone();
        b.iter(|| {
            item.clone().verify_single(pvk).expect("valid proof");
        })
    });

    // Multiple proofs verified individually (no batching).
    // Items are cycled to reach batch sizes larger than the source set.
    // This measures linear scaling since Zebra does not yet batch Groth16
    // (see https://github.com/ZcashFoundation/zebra/issues/3127).
    for n in [2, 4, 8, 16, 32, 64] {
        let items = cycled_items(&source_items, n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new("unbatched", n),
            &items,
            |b, items: &Vec<Item>| {
                b.iter(|| {
                    for item in items.iter() {
                        item.clone().verify_single(pvk).expect("valid proof");
                    }
                })
            },
        );
    }

    group.finish();

    // -- Input preparation benchmarks --

    let mut group = c.benchmark_group("Groth16 Input Preparation");

    for (_height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes.zcash_deserialize_into().expect("valid block");

        for tx in &block.transactions {
            let joinsplits: Vec<&JoinSplit<Groth16Proof>> =
                tx.sprout_groth16_joinsplits().collect();

            if joinsplits.is_empty() {
                continue;
            }

            let pub_key = tx
                .sprout_joinsplit_pub_key()
                .expect("pub key must exist since there are joinsplits");

            let joinsplit = joinsplits[0];

            // Cost of computing h_sig + encoding primary inputs as BLS12-381 scalars.
            group.bench_function("primary_inputs", |b| {
                b.iter(|| {
                    let description: (&JoinSplit<Groth16Proof>, &_) = (joinsplit, &pub_key);
                    description.primary_inputs()
                })
            });

            // Cost of proof deserialization (Proof::read on 192 bytes) + input computation.
            group.bench_function("item_creation", |b| {
                b.iter(|| {
                    let _item: Item = DescriptionWrapper(&(joinsplit, &pub_key))
                        .try_into()
                        .expect("valid groth16 item");
                })
            });

            group.finish();
            return;
        }
    }
}

criterion_group!(benches, bench_groth16_verify);
criterion_main!(benches);
