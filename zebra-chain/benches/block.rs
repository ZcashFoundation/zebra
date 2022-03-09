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
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.05).sample_size(50);
    targets = block_serialization
);
criterion_main!(benches);
