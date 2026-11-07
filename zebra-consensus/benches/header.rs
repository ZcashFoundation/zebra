//! Benchmarks for context-free header verification, the proof of work checks
//! that headers-first sync runs on every header before it stores it.
//!
//! # Throughput dimension
//!
//! Reports headers per second (via [`Throughput::Elements`]): one header is one
//! Equihash verification, one header hash and one difficulty filter check.
//!
//! The test vectors are not contiguous, so each header is verified as its own
//! batch at its own height. `parallel` spreads a 160-header response over the
//! rayon thread pool, as `verify_headers` does; `single` is one core.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

mod common;

use std::sync::Arc;

use chrono::Utc;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rayon::prelude::*;

use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::header::verify_headers_blocking;

/// The Mainnet test vector headers with their heights.
fn mainnet_headers() -> Vec<(Height, Arc<block::Header>)> {
    zebra_test::vectors::MAINNET_BLOCKS
        .iter()
        .map(|(height, bytes)| {
            let block: Block = bytes.zcash_deserialize_into().expect("valid block");
            (Height(*height), block.header)
        })
        .collect()
}

fn bench_header_verification(c: &mut Criterion) {
    let network = Network::Mainnet;
    let now = Utc::now();
    // One response: the most headers a peer sends at once.
    let batch = common::cycled(&mainnet_headers(), 160);

    let mut group = c.benchmark_group("header_verification");
    // The batch length is a small constant, so it fits in a u64.
    group.throughput(Throughput::Elements(batch.len() as u64));

    group.bench_with_input(
        BenchmarkId::new("single", batch.len()),
        &batch,
        |b, batch| {
            b.iter(|| {
                for (height, header) in batch {
                    verify_headers_blocking(&network, *height, std::slice::from_ref(header), now)
                        .expect("test vector headers are valid");
                }
            })
        },
    );

    group.bench_with_input(
        BenchmarkId::new("parallel", batch.len()),
        &batch,
        |b, batch| {
            b.iter(|| {
                batch.par_iter().for_each(|(height, header)| {
                    verify_headers_blocking(&network, *height, std::slice::from_ref(header), now)
                        .expect("test vector headers are valid");
                })
            })
        },
    );

    group.finish();
}

criterion_group!(benches, bench_header_verification);
criterion_main!(benches);
