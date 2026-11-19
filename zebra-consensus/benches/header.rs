//! Benchmarks for context-free header verification, the proof of work checks
//! that headers-first sync runs on every header before it stores it.
//!
//! # Throughput dimension
//!
//! Reports headers per second (via [`Throughput::Elements`]): one header is one
//! Equihash verification, one header hash and one difficulty filter check.
//!
//! Inputs are distinct, contiguous Mainnet headers from the Sprout and NU5 eras.
//! Fixture decoding and validation run before timing. Each iteration calls the
//! production batch verifier once, including its contiguity and checkpoint checks.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::{sync::Arc, time::Duration};

use chrono::DateTime;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};

use zebra_chain::{
    block::{self, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::header::{verify_headers, verify_headers_blocking};

/// Decode a full response and anchor it to an existing block fixture, outside timing.
fn load_headers(height: u32, hex: &str) -> Vec<Arc<block::Header>> {
    let headers: Vec<Arc<block::Header>> = hex
        .lines()
        .map(|line| {
            let bytes = hex::decode(line).expect("fixture headers are hexadecimal");
            Arc::new(
                bytes
                    .zcash_deserialize_into()
                    .expect("valid fixture header"),
            )
        })
        .collect();
    let anchor: block::Header = zebra_test::vectors::MAINNET_BLOCKS[&height]
        .zcash_deserialize_into()
        .expect("the existing anchor block starts with a valid header");
    assert_eq!(headers.len(), 160, "each fixture is one full peer response");
    assert_eq!(
        headers[0].as_ref(),
        &anchor,
        "the range starts at a known block"
    );
    headers
}

fn bench_header_verification(c: &mut Criterion) {
    let workers = rayon::current_num_threads();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("the benchmark runtime can be created");
    let network = Network::Mainnet;
    // 2023-11-14, after both historical fixture ranges; independent of wall time.
    let now = DateTime::from_timestamp(1_700_000_000, 0)
        .expect("the fixed benchmark timestamp is representable");

    for &(height, hex) in zebra_test::vectors::MAINNET_HEADER_BATCHES {
        let headers = load_headers(height, hex);
        let first_height = Height(height);
        let expected_hashes: Vec<_> = headers.iter().map(|header| header.hash()).collect();
        // The async API calls the same blocking verifier measured below.
        // Validate the complete fixture once, before registering timed cases.
        let async_hashes = runtime
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(30),
                    verify_headers(&network, first_height, headers.clone(), now),
                )
                .await
                .expect("fixture verification completes within 30 seconds")
            })
            .expect("the async production verifier accepts the complete fixture batch");
        assert_eq!(async_hashes, expected_hashes);

        let mut group = c.benchmark_group(format!("header_verification/mainnet-{height}"));
        for batch_size in [1, 16, 160] {
            let batch = &headers[..batch_size];
            group.throughput(Throughput::Elements(
                u64::try_from(batch_size).expect("batch sizes are at most 160"),
            ));
            group.bench_function(
                BenchmarkId::new(format!("blocking-workers-{workers}"), batch_size),
                |b| {
                    b.iter(|| {
                        verify_headers_blocking(&network, first_height, batch, now)
                            .expect("fixture headers pass production batch verification")
                    })
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("async-workers-{workers}"), batch_size),
                |b| {
                    b.iter_batched(
                        || batch.to_vec(),
                        |owned_batch| {
                            let hashes = runtime
                                .block_on(verify_headers(&network, first_height, owned_batch, now))
                                .expect("fixture headers pass production async verification");
                            drop(std::hint::black_box(hashes));
                        },
                        BatchSize::PerIteration,
                    )
                },
            );
        }
        group.finish();
    }
}

criterion_group!(benches, bench_header_verification);
criterion_main!(benches);
