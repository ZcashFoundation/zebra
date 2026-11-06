// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::io::Cursor;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use zebra_chain::{
    block::{
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
            many_transaction_v4_block,
        },
        Block,
    },
    serialization::{ZcashDeserialize, ZcashSerialize},
};
use zebra_test::vectors::BLOCK_TESTNET_141042_BYTES;

fn block_serialization(c: &mut Criterion) {
    // Biggest block from `zebra-test`.
    let block141042_bytes: &[u8] = BLOCK_TESTNET_141042_BYTES.as_ref();
    let block141042 = Block::zcash_deserialize(Cursor::new(block141042_bytes)).unwrap();

    let blocks = vec![
        ("BLOCK_TESTNET_141042", block141042),
        (
            "large_multi_transaction_block",
            large_multi_transaction_block(),
        ),
        (
            "large_single_transaction_block_many_inputs",
            large_single_transaction_block_many_inputs(),
        ),
    ];

    for (name, block) in blocks {
        c.bench_with_input(
            BenchmarkId::new("zcash_serialize_to_vec", name),
            &block,
            |b, block| b.iter(|| block.zcash_serialize_to_vec().unwrap()),
        );

        let block_bytes = block.zcash_serialize_to_vec().unwrap();
        c.bench_with_input(
            BenchmarkId::new("zcash_deserialize", name),
            &block_bytes,
            |b, bytes| b.iter(|| Block::zcash_deserialize(Cursor::new(bytes)).unwrap()),
        );
    }

    let bytes = many_transaction_v4_block(9_000);
    let mut group = c.benchmark_group("decode_1900kb_v4");
    group.throughput(criterion::Throughput::Bytes(
        u64::try_from(bytes.len()).unwrap(),
    ));
    group.bench_function("serial", |b| {
        b.iter(|| Block::zcash_deserialize(std::hint::black_box(bytes.as_slice())).unwrap())
    });
    group.bench_function("parallel", |b| {
        b.iter(|| {
            Block::zcash_deserialize_parallel(&mut std::hint::black_box(bytes.as_slice())).unwrap()
        })
    });
    group.finish();

    let mut malformed = many_transaction_v4_block(9_000);
    let start = zebra_chain::block::tests::generate::block_header().1.len() + 3;
    malformed[start + 200] = 1; // invalid valueBalanceSapling in the first transaction
    let mut group = c.benchmark_group("decode_malformed_1900kb_v4");
    group.bench_function("serial", |b| {
        b.iter(|| Block::zcash_deserialize(std::hint::black_box(malformed.as_slice())).unwrap_err())
    });
    group.bench_function("parallel", |b| {
        b.iter(|| {
            Block::zcash_deserialize_parallel(&mut std::hint::black_box(malformed.as_slice()))
                .unwrap_err()
        })
    });
    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.05).sample_size(50);
    targets = block_serialization
);
criterion_main!(benches);
