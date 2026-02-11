// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::io::Cursor;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use zebra_chain::{
    block::{
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
        },
        Block,
    },
    serialization::{TrustedDeserializationGuard, ZcashDeserialize, ZcashSerialize},
};
use zebra_test::vectors::{
    BLOCK_MAINNET_419201_BYTES, BLOCK_MAINNET_903000_BYTES, BLOCK_MAINNET_1046401_BYTES,
    BLOCK_TESTNET_141042_BYTES,
};

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
}

fn trusted_vs_untrusted_deserialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("trusted_deserialization");
    group.noise_threshold(0.05);
    group.sample_size(50);

    let blocks: Vec<(&str, &[u8])> = vec![
        // First post-Sapling block (height 419201)
        ("mainnet_419201", BLOCK_MAINNET_419201_BYTES.as_ref()),
        // Heartwood activation block (height 903000)
        ("mainnet_903000", BLOCK_MAINNET_903000_BYTES.as_ref()),
        // Post-Canopy block (height 1046401, largest available test vector)
        ("mainnet_1046401", BLOCK_MAINNET_1046401_BYTES.as_ref()),
    ];

    for (name, block_bytes) in blocks {
        group.bench_with_input(
            BenchmarkId::new("untrusted", name),
            &block_bytes,
            |b, bytes| {
                b.iter(|| Block::zcash_deserialize(Cursor::new(bytes)).unwrap());
            },
        );

        group.bench_with_input(
            BenchmarkId::new("trusted", name),
            &block_bytes,
            |b, bytes| {
                b.iter(|| {
                    let _guard = TrustedDeserializationGuard::new();
                    Block::zcash_deserialize(Cursor::new(bytes)).unwrap()
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.05).sample_size(50);
    targets = block_serialization, trusted_vs_untrusted_deserialization
);
criterion_main!(benches);
