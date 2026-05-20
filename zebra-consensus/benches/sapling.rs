//! Benchmarks for Sapling proof and signature verification.
//!
//! Measures the cost of verifying Sapling shielded transaction data: Groth16
//! spend/output proofs plus binding/spend-auth signatures. Uses
//! `sapling_crypto::BatchValidator` directly, which supports true batch
//! verification across bundles.
//!
//! # Benchmark group
//!
//! `groth16_sapling`: matches the `verifier` label in
//! `zebra.consensus.batch.duration_seconds` emitted from
//! `zebra-consensus/src/primitives/sapling.rs`, so a prod regression on the
//! Sapling histogram maps to this file by name.
//!
//! # Batched vs unbatched
//!
//! The `batched` series is the production path; the `unbatched` series is
//! the fallback taken when a batch fails verification and items are
//! re-verified individually.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

mod common;

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::thread_rng;

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::{HashType, SigHash},
    transparent,
};

use sapling_crypto::{bundle::Authorized, BatchValidator, Bundle};
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::value::ZatBalance;

/// A Sapling bundle paired with its transaction sighash, ready for verification.
#[derive(Clone)]
struct SaplingItem {
    bundle: Bundle<Authorized, ZatBalance>,
    sighash: SigHash,
}

/// Extracts valid Sapling bundles and sighashes from mainnet test blocks.
///
/// Transactions with transparent inputs are skipped because their sighash
/// computation requires previous outputs not available in the test vectors.
fn extract_sapling_items_from_blocks() -> Vec<SaplingItem> {
    let mut items = Vec::new();

    let network = Network::Mainnet;

    for (height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes.zcash_deserialize_into().expect("valid block");
        let height = block::Height(*height);

        for tx in &block.transactions {
            if !tx.has_sapling_shielded_data() {
                continue;
            }

            if !tx.inputs().is_empty() {
                continue;
            }

            // Use the correct network upgrade for this block height so the
            // sighash is computed with the right consensus branch ID.
            let nu = NetworkUpgrade::current(&network, height);

            let all_previous_outputs: Arc<Vec<transparent::Output>> = Arc::new(Vec::new());

            let Ok(sighasher) = tx.sighasher(nu, all_previous_outputs) else {
                continue;
            };

            let Some(bundle) = sighasher.sapling_bundle() else {
                continue;
            };

            let sighash = sighasher.sighash(HashType::ALL, None);

            items.push(SaplingItem { bundle, sighash });
        }
    }

    assert!(
        !items.is_empty(),
        "Sapling-era test blocks must contain Sapling transactions without transparent inputs"
    );

    items
}

fn bench_sapling_verify(c: &mut Criterion) {
    let sapling = LocalTxProver::bundled();
    let (spend_vk, output_vk) = sapling.verifying_keys();
    let source_items = extract_sapling_items_from_blocks();

    let mut group = c.benchmark_group("groth16_sapling");

    group.throughput(Throughput::Elements(1));
    group.bench_function("single bundle", |b| {
        let item = source_items[0].clone();
        b.iter(|| {
            let mut batch = BatchValidator::default();
            assert!(batch.check_bundle(item.bundle.clone(), item.sighash.into()));
            assert!(batch.validate(&spend_vk, &output_vk, thread_rng()));
        })
    });

    for n in [2, 4, 8, 16, 32, 64] {
        let items = common::cycled(&source_items, n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new("unbatched", n),
            &items,
            |b, items: &Vec<SaplingItem>| {
                b.iter(|| {
                    for item in items {
                        let mut batch = BatchValidator::default();
                        assert!(batch.check_bundle(item.bundle.clone(), item.sighash.into()));
                        assert!(batch.validate(&spend_vk, &output_vk, thread_rng()));
                    }
                })
            },
        );
    }

    // All bundles share one BatchValidator, amortizing the final
    // multi-scalar multiplication.
    for n in [2, 4, 8, 16, 32, 64] {
        let items = common::cycled(&source_items, n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new("batched", n),
            &items,
            |b, items: &Vec<SaplingItem>| {
                b.iter(|| {
                    let mut batch = BatchValidator::default();
                    for item in items {
                        assert!(batch.check_bundle(item.bundle.clone(), item.sighash.into()));
                    }
                    assert!(batch.validate(&spend_vk, &output_vk, thread_rng()));
                })
            },
        );
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().noise_threshold(0.1).sample_size(50);
    targets = bench_sapling_verify
}
criterion_main!(benches);
