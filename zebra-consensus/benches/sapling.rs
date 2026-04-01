//! Benchmarks for Sapling proof and signature verification.
//!
//! These benchmarks measure the cost of verifying Sapling shielded transaction
//! data, which includes Groth16 spend/output proofs and binding/spend-auth
//! signatures. Sapling verification uses `sapling_crypto::BatchValidator`, which
//! supports true batch verification across multiple bundles.
//!
//! # Test data limitations
//!
//! The hardcoded mainnet test blocks contain only a small number of Sapling
//! transactions. Items are cycled (repeated) to fill larger batch sizes.
//!
//! Sapling verification cost depends on the number of spends and outputs in each
//! bundle. Cycling reuses the same bundles, so per-bundle cost is representative
//! but cache effects may differ from a real workload with unique proofs.
//!
//! # Batched vs unbatched
//!
//! Unlike the Halo2 benchmark, this benchmark can exercise true batch
//! verification because we call `sapling_crypto::BatchValidator` directly with
//! the public verifying keys. This measures the actual production verification
//! path, including the speedup from batching multiple bundles together.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

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
use zcash_protocol::value::ZatBalance;
use zebra_consensus::groth16::SAPLING;

/// A Sapling bundle paired with its transaction sighash, ready for verification.
#[derive(Clone)]
struct SaplingItem {
    bundle: Bundle<Authorized, ZatBalance>,
    sighash: SigHash,
}

/// Extracts valid Sapling bundles and sighashes from mainnet test blocks.
///
/// Iterates over `MAINNET_BLOCKS`, deserializes each block, and for each
/// transaction with Sapling shielded data, converts it to a librustzcash
/// representation to extract the `sapling_crypto::Bundle` and compute the
/// sighash.
///
/// Transactions with transparent inputs are skipped because their sighash
/// computation requires previous outputs not available in the test vectors.
///
/// Returns a small set of real, valid Sapling items from current test vectors.
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

            // Skip transactions with transparent inputs: we don't have their
            // previous outputs in the test vectors.
            if !tx.inputs().is_empty() {
                continue;
            }

            // Use the correct network upgrade for this block height so the
            // sighash is computed with the right consensus branch ID.
            let nu = NetworkUpgrade::current(&network, height);

            let all_previous_outputs: Arc<Vec<transparent::Output>> = Arc::new(Vec::new());

            let sighasher = match tx.sighasher(nu, all_previous_outputs) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let bundle = match sighasher.sapling_bundle() {
                Some(b) => b,
                None => continue,
            };

            let sighash = sighasher.sighash(HashType::ALL, None);

            items.push(SaplingItem { bundle, sighash });
        }
    }

    eprintln!("Extracted {} Sapling items from test blocks", items.len());

    assert!(
        !items.is_empty(),
        "Sapling-era test blocks must contain Sapling transactions without transparent inputs"
    );

    items
}

/// Creates a batch of `n` items by cycling through the source items.
///
/// Sapling verification cost depends on the number of spends and outputs in
/// each bundle. Cycling reuses the same proof structures, so per-bundle cost
/// is representative but cache effects are not.
fn cycled_items(source: &[SaplingItem], n: usize) -> Vec<SaplingItem> {
    source.iter().cycle().take(n).cloned().collect()
}

/// Benchmarks Sapling proof verification at various batch sizes, comparing
/// single verification (one-item batches) against true batch verification
/// (multiple bundles validated together).
fn bench_sapling_verify(c: &mut Criterion) {
    let (spend_vk, output_vk) = SAPLING.verifying_keys();
    let source_items = extract_sapling_items_from_blocks();

    let mut group = c.benchmark_group("Sapling Verification");

    // Single bundle verification: creates a BatchValidator with one item.
    group.throughput(Throughput::Elements(1));
    group.bench_function("single bundle", |b| {
        let item = source_items[0].clone();
        b.iter(|| {
            let mut batch = BatchValidator::default();
            assert!(batch.check_bundle(item.bundle.clone(), item.sighash.into()));
            assert!(batch.validate(&spend_vk, &output_vk, thread_rng()));
        })
    });

    // Multiple bundles verified individually (one-item batch per bundle).
    for n in [2, 4, 8, 16, 32, 64] {
        let items = cycled_items(&source_items, n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new("unbatched", n),
            &items,
            |b, items: &Vec<SaplingItem>| {
                b.iter(|| {
                    for item in items.iter() {
                        let mut batch = BatchValidator::default();
                        assert!(batch.check_bundle(item.bundle.clone(), item.sighash.into()));
                        assert!(batch.validate(&spend_vk, &output_vk, thread_rng()));
                    }
                })
            },
        );
    }

    // True batch verification: all bundles in a single BatchValidator.
    // This is the production path — bundles are accumulated and validated
    // together, amortizing the cost of the final multi-scalar multiplication.
    for n in [2, 4, 8, 16, 32, 64] {
        let items = cycled_items(&source_items, n);

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new("batched", n),
            &items,
            |b, items: &Vec<SaplingItem>| {
                b.iter(|| {
                    let mut batch = BatchValidator::default();
                    for item in items.iter() {
                        assert!(batch.check_bundle(item.bundle.clone(), item.sighash.into()));
                    }
                    assert!(batch.validate(&spend_vk, &output_vk, thread_rng()));
                })
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_sapling_verify);
criterion_main!(benches);
