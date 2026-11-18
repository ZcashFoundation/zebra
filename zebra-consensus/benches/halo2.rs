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
//! # Batched and CPU-cache comparisons
//!
//! `halo2_cpu` keeps each Orchard bundle and sighash alongside the `Item`
//! because `Item`'s fields are private. It uses `orchard::bundle::BatchValidator`
//! directly for true one- and four-bundle batches with hot or cold CPU caches,
//! plus four concurrent four-bundle batches.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

mod common;

use std::{
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use orchard::bundle::{Authorized, BatchValidator, Bundle};
use zcash_protocol::value::ZatBalance;

use zebra_chain::{
    block::Block, parameters::NetworkUpgrade, serialization::ZcashDeserializeInto,
    transaction::SigHash, transparent,
};

use tower_batch_control::RequestWeight;
use zebra_consensus::halo2::{Item, VERIFYING_KEY_PRE_NU6_2};

/// A bundle and its sighash, alongside the cheap-to-clone fallback item.
#[derive(Clone)]
struct Halo2BenchmarkItem {
    bundle: Bundle<Authorized, ZatBalance>,
    sighash: SigHash,
    item: Item,
}

/// Extracts valid Halo2 items (Orchard bundles + sighashes) from NU5+ mainnet
/// test blocks.
///
/// Transactions with transparent inputs are skipped because computing their
/// sighash requires the previous outputs they spend, which are not available
/// in the test vectors. Orchard-only and Sapling-to-Orchard transactions
/// work with an empty previous outputs set.
fn extract_halo2_items_from_blocks() -> Vec<Halo2BenchmarkItem> {
    let mut items = Vec::new();

    for bytes in zebra_test::vectors::MAINNET_BLOCKS.values() {
        let block: Block = bytes.zcash_deserialize_into().expect("valid block");

        for tx in &block.transactions {
            if !tx.has_orchard_shielded_data() {
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

            // These mainnet test blocks are NU5-era Orchard history (mined before NU6.2),
            // so they verify under the pre-NU6.2 verifying key (see the `verify_single` calls
            // below, which pass `VERIFYING_KEY_PRE_NU6_2`).
            items.push(Halo2BenchmarkItem {
                item: Item::new(bundle.clone(), sighash),
                bundle,
                sighash,
            });
        }
    }

    assert!(
        !items.is_empty(),
        "NU5+ test blocks must contain Orchard transactions without transparent inputs"
    );

    items
}

fn bench_halo2_verify(c: &mut Criterion) {
    let source_items: Vec<_> = extract_halo2_items_from_blocks()
        .into_iter()
        .map(|source| source.item)
        .collect();

    let mut group = c.benchmark_group("halo2");

    group.throughput(Throughput::Elements(source_items[0].request_weight() as u64));
    group.bench_function("single bundle", |b| {
        let item = source_items[0].clone();
        b.iter(|| {
            assert!(item.clone().verify_single(&VERIFYING_KEY_PRE_NU6_2));
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
                        assert!(item.clone().verify_single(&VERIFYING_KEY_PRE_NU6_2));
                    }
                })
            },
        );
    }

    group.finish();
}

fn verify_batch(items: &[Halo2BenchmarkItem]) {
    let mut batch = BatchValidator::new(&VERIFYING_KEY_PRE_NU6_2);
    for item in items {
        batch
            .add_bundle(&item.bundle, item.sighash.0)
            .expect("test-vector Orchard bundle is valid for the pre-NU6.2 key");
    }
    assert!(
        batch.validate(rand::rng()),
        "test-vector Orchard proofs verify"
    );
}

// Match the production verifier: validate on a Rayon worker, not the Criterion thread.
fn verify_batch_on_pool(
    items: Arc<Vec<Halo2BenchmarkItem>>,
    mut eviction: Vec<u8>,
) -> (Duration, Vec<u8>) {
    let (tx, rx) = mpsc::sync_channel(1);
    rayon::spawn_fifo(move || {
        for byte in eviction.iter_mut().step_by(64) {
            *byte = byte.wrapping_add(1);
        }
        std::hint::black_box(&eviction);
        let start = Instant::now();
        verify_batch(&items);
        let _ = tx.send((start.elapsed(), eviction));
    });
    rx.recv().expect("Rayon worker completed verification")
}

fn bench_halo2_cpu(c: &mut Criterion) {
    let source_items = extract_halo2_items_from_blocks();
    let mut group = c.benchmark_group("halo2_cpu");

    for n in [1, 4] {
        let items = Arc::new(common::cycled(&source_items, n));
        let actions = items
            .iter()
            .map(|item| item.bundle.actions().len() as u64)
            .sum();
        group.throughput(Throughput::Elements(actions));
        group.bench_with_input(BenchmarkId::new("hot", n), &items, |b, items| {
            b.iter_custom(|iterations| {
                (0..iterations)
                    .map(|_| verify_batch_on_pool(Arc::clone(items), Vec::new()).0)
                    .sum()
            });
        });

        group.bench_with_input(BenchmarkId::new("cold", n), &items, |b, items| {
            // Evict on the worker itself before timing, including its local V-Cache.
            let mut eviction = vec![0u8; 128 * 1024 * 1024];
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let (duration, cache) =
                        verify_batch_on_pool(Arc::clone(items), std::mem::take(&mut eviction));
                    eviction = cache;
                    elapsed += duration;
                }
                elapsed
            });
        });
    }

    let items = Arc::new(common::cycled(&source_items, 4));
    let actions: u64 = items
        .iter()
        .map(|item| item.bundle.actions().len() as u64)
        .sum();
    group.throughput(Throughput::Elements(actions * 4));
    group.bench_function("concurrent_4x4", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::sync_channel(4);
            for _ in 0..4 {
                let tx = tx.clone();
                let items = Arc::clone(&items);
                rayon::spawn_fifo(move || {
                    verify_batch(&items);
                    let _ = tx.send(());
                });
            }
            drop(tx);
            for _ in 0..4 {
                rx.recv().expect("Rayon worker completed verification");
            }
        });
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().noise_threshold(0.1).sample_size(50);
    targets = bench_halo2_verify, bench_halo2_cpu
}
criterion_main!(benches);

/// Runs the same benchmarks after zebrad initializes its configured Rayon pool.
#[allow(dead_code)]
pub(crate) fn run_benchmarks() {
    benches();
    Criterion::default().configure_from_args().final_summary();
}
