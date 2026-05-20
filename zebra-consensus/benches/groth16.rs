//! Benchmarks for Groth16 proof verification (Sprout JoinSplits).
//!
//! Groth16 verification is a pairing check on BLS12-381. Sprout has no batch
//! verifier in Zebra; each proof is verified individually, so the only
//! throughput dimension is per-proof cost. Test vectors contain few
//! JoinSplits, so items are cycled to fill larger sizes.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

mod common;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use bellman::groth16::PreparedVerifyingKey;
use bls12_381::Bls12;

use zebra_chain::{
    block::Block,
    primitives::{ed25519, Groth16Proof},
    serialization::ZcashDeserializeInto,
    sprout::JoinSplit,
};

use zebra_consensus::groth16::{Item, SPROUT};

/// A Sprout JoinSplit paired with its transaction's JoinSplit public key,
/// ready to be converted into a Groth16 verification [`Item`].
#[derive(Clone)]
struct JoinSplitSource {
    joinsplit: JoinSplit<Groth16Proof>,
    pub_key: ed25519::VerificationKeyBytes,
}

/// Extracts JoinSplit/pub-key pairs from the mainnet test blocks.
///
/// Only V4 transactions contain Groth16 JoinSplits (V2/V3 use BCTV14 proofs,
/// V5+ dropped Sprout support). The returned pairs are the raw inputs to
/// [`Item`] construction and to `primary_inputs()`.
fn extract_joinsplit_sources() -> Vec<JoinSplitSource> {
    let mut sources = Vec::new();

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
                sources.push(JoinSplitSource {
                    joinsplit: joinsplit.clone(),
                    pub_key,
                });
            }
        }
    }

    assert!(
        !sources.is_empty(),
        "test blocks must contain Groth16 JoinSplits"
    );
    sources
}

/// Converts a [`JoinSplitSource`] into a verification [`Item`].
fn item_from(source: &JoinSplitSource) -> Item {
    Item::from_joinsplit(&source.joinsplit, &source.pub_key).expect("valid groth16 item")
}

fn bench_groth16_verify(c: &mut Criterion) {
    let pvk: &'static PreparedVerifyingKey<Bls12> = SPROUT.prepared_verifying_key();
    let sources = extract_joinsplit_sources();
    let items: Vec<Item> = sources.iter().map(item_from).collect();

    let mut group = c.benchmark_group("Groth16 Verification");

    group.throughput(Throughput::Elements(1));
    group.bench_function("single proof", |b| {
        let item = items[0].clone();
        b.iter(|| {
            item.clone().verify_single(pvk).expect("valid proof");
        })
    });

    for n in [2, 4, 8, 16, 32, 64] {
        let batch = common::cycled(&items, n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("unbatched", n), &batch, |b, batch| {
            b.iter(|| {
                for item in batch {
                    item.clone().verify_single(pvk).expect("valid proof");
                }
            })
        });
    }

    group.finish();
}

fn bench_groth16_inputs(c: &mut Criterion) {
    let sources = extract_joinsplit_sources();
    let source = sources.first().expect("at least one JoinSplit source");

    let mut group = c.benchmark_group("Groth16 Input Preparation");

    // Cost of Proof::read (192 bytes) + primary input computation, i.e. the
    // full Item construction that runs before verification.
    group.bench_function("item_creation", |b| b.iter(|| item_from(source)));

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().noise_threshold(0.1).sample_size(50);
    targets = bench_groth16_verify, bench_groth16_inputs
}
criterion_main!(benches);
