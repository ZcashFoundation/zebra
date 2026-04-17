//! Benchmarks for Halo2 proof verification (Orchard Actions).
//!
//! These benchmarks measure the cost of verifying Halo2 zero-knowledge proofs
//! used in Orchard Action descriptions. Unlike Groth16 (which uses a pairing
//! check), Halo2 uses an inner product argument on the Pallas curve. Each
//! Orchard bundle contains an aggregate proof covering all its actions.
//!
//! # Test data limitations
//!
//! The hardcoded NU5 mainnet test blocks contain only a small number of Orchard
//! transactions. Items are cycled (repeated) to fill larger batch sizes when
//! needed.
//!
//! Halo2 verification cost scales with the number of actions in the aggregate
//! proof, not just the number of bundles. The benchmarks report both bundle
//! count and total action count for context.
//!
//! Since we reuse the same bundles, cache effects and memory access patterns
//! may differ from a real workload with unique proofs.
//!
//! # Batched vs unbatched
//!
//! The `orchard::bundle::BatchValidator` supports true batch verification, but
//! `Item`'s internal fields are private, so the benchmark can only exercise
//! `verify_single()` (which creates a one-item batch internally). This still
//! measures the dominant cost — the Halo2 proof verification itself — and
//! provides a useful baseline. To benchmark cross-bundle batching, a public
//! batch API would need to be exposed on `Item`.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::sync::Arc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use zebra_chain::{
    block::Block, parameters::NetworkUpgrade, serialization::ZcashDeserializeInto, transparent,
};

use tower_batch_control::RequestWeight;
use zebra_consensus::halo2::{Item, VERIFYING_KEY};

/// Extracts valid Halo2 items (Orchard bundles + sighashes) from NU5+ mainnet
/// test blocks.
///
/// Iterates over `MAINNET_BLOCKS`, deserializes each block, and for each V5
/// transaction with Orchard shielded data, converts it to a librustzcash
/// representation to extract the `orchard::bundle::Bundle` and compute the
/// transaction sighash.
///
/// Transactions with transparent inputs are skipped because computing their
/// sighash requires the previous outputs they spend, which are not available
/// in the test vectors. Orchard-only and Sapling-to-Orchard transactions
/// work with an empty previous outputs set.
///
/// Returns a small set of real, valid Halo2 items from current test vectors.
fn extract_halo2_items_from_blocks() -> Vec<Item> {
    let mut items = Vec::new();

    for (_height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes.zcash_deserialize_into().expect("valid block");

        for tx in &block.transactions {
            if tx.orchard_shielded_data().is_none() {
                continue;
            }

            // Skip transactions with transparent inputs: we don't have their
            // previous outputs in the test vectors, so we can't compute the
            // sighash correctly.
            if !tx.inputs().is_empty() {
                continue;
            }

            let all_previous_outputs: Arc<Vec<transparent::Output>> = Arc::new(Vec::new());

            let sighasher = match tx.sighasher(NetworkUpgrade::Nu5, all_previous_outputs) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let bundle = match sighasher.orchard_bundle() {
                Some(b) => b,
                None => continue,
            };

            let sighash = sighasher.sighash(zebra_chain::transaction::HashType::ALL, None);

            items.push(Item::new(bundle, sighash));
        }
    }

    assert!(
        !items.is_empty(),
        "NU5+ test blocks must contain Orchard transactions without transparent inputs"
    );

    items
}

/// Creates a batch of `n` items by cycling through the source items.
///
/// Halo2 verification cost depends on the number of actions in each bundle's
/// aggregate proof. Cycling reuses the same proof structures, so the per-action
/// cost is representative but cache effects are not.
fn cycled_items(source: &[Item], n: usize) -> Vec<Item> {
    source.iter().cycle().take(n).cloned().collect()
}

/// Benchmarks Halo2 proof verification at various batch sizes.
///
/// Each call to `verify_single` internally creates a `BatchValidator` with one
/// item and runs the full verification circuit. This is the per-bundle cost and
/// matches the fallback path used when batch verification fails in production.
fn bench_halo2_verify(c: &mut Criterion) {
    let vk = &*VERIFYING_KEY;
    let source_items = extract_halo2_items_from_blocks();

    let mut group = c.benchmark_group("Halo2 Verification");

    // Single bundle verification.
    group.throughput(Throughput::Elements(1));
    group.bench_function("single bundle", |b| {
        let item = source_items[0].clone();
        b.iter(|| {
            assert!(item.clone().verify_single(vk));
        })
    });

    // Multiple bundles verified individually (no cross-bundle batching).
    // Each call to verify_single creates its own BatchValidator.
    for n in [2, 4, 8, 16, 32] {
        let items = cycled_items(&source_items, n);
        let total_actions: usize = items.iter().map(|i| i.request_weight()).sum();

        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("unbatched ({total_actions} actions)"), n),
            &items,
            |b, items: &Vec<Item>| {
                b.iter(|| {
                    for item in items.iter() {
                        assert!(item.clone().verify_single(vk));
                    }
                })
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_halo2_verify);
criterion_main!(benches);
