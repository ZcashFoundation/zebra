//! Benchmarks for Halo2 proof verification (Orchard Actions).
//!
//! Measures the cost of verifying Halo2 zero-knowledge proofs used in Orchard
//! Action descriptions. Halo2 uses an inner product argument on the Pallas
//! curve; each bundle contains an aggregate proof covering all its actions.
//!
//! # Benchmark group
//!
//! `halo2`: matches the `verifier` label in
//! `zebra.consensus.batch.duration_seconds` emitted from
//! `zebra-consensus/src/primitives/halo2.rs`, so a prod regression on the
//! Halo2 histogram maps to this file by name.
//!
//! # Throughput dimension
//!
//! Halo2 verification cost scales with the number of actions in the aggregate
//! proof, not the number of bundles. The benchmark reports throughput in
//! actions (via [`Throughput::Elements`]) so the dashboard shows
//! time-per-action — the unit that actually matters for capacity planning.
//!
//! # Batching
//!
//! `orchard::bundle::BatchValidator` supports cross-bundle batching, but
//! [`Item`]'s internal fields are private, so this benchmark can only
//! exercise `verify_single()` (a one-item batch).

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

mod common;

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
/// Transactions with transparent inputs are skipped because computing their
/// sighash requires the previous outputs they spend, which are not available
/// in the test vectors. Orchard-only and Sapling-to-Orchard transactions
/// work with an empty previous outputs set.
fn extract_halo2_items_from_blocks() -> Vec<Item> {
    let mut items = Vec::new();

    for (_height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes.zcash_deserialize_into().expect("valid block");

        for tx in &block.transactions {
            if tx.orchard_shielded_data().is_none() {
                continue;
            }

            if !tx.inputs().is_empty() {
                continue;
            }

            let all_previous_outputs: Arc<Vec<transparent::Output>> = Arc::new(Vec::new());

            let Ok(sighasher) = tx.sighasher(NetworkUpgrade::Nu5, all_previous_outputs) else {
                continue;
            };

            let Some(bundle) = sighasher.orchard_bundle() else {
                continue;
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

fn bench_halo2_verify(c: &mut Criterion) {
    let vk = &*VERIFYING_KEY;
    let source_items = extract_halo2_items_from_blocks();

    let mut group = c.benchmark_group("halo2");

    group.throughput(Throughput::Elements(source_items[0].request_weight() as u64));
    group.bench_function("single bundle", |b| {
        let item = source_items[0].clone();
        b.iter(|| {
            assert!(item.clone().verify_single(vk));
        })
    });

    for n in [2, 4, 8, 16, 32] {
        let items = common::cycled(&source_items, n);
        // Scale throughput by total actions, not bundles, so the dashboard
        // series stays stable across test-vector changes.
        let total_actions: u64 = items.iter().map(|i| i.request_weight() as u64).sum();

        group.throughput(Throughput::Elements(total_actions));
        group.bench_with_input(
            BenchmarkId::new("unbatched", n),
            &items,
            |b, items: &Vec<Item>| {
                b.iter(|| {
                    for item in items {
                        assert!(item.clone().verify_single(vk));
                    }
                })
            },
        );
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().noise_threshold(0.1).sample_size(50);
    targets = bench_halo2_verify
}
criterion_main!(benches);
