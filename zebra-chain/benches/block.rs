use std::io::Cursor;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use zebra_chain::{
    block::Block,
    serialization::{ZcashDeserialize, ZcashSerialize},
};
use zebra_test::vectors::BLOCK_TESTNET_141042_BYTES;

fn block_serialization(c: &mut Criterion) {
    // Biggest block from `zebra-test`.
    let block_bytes: &[u8] = BLOCK_TESTNET_141042_BYTES.as_ref();
    let block = Block::zcash_deserialize(Cursor::new(block_bytes)).unwrap();

    c.bench_with_input(
        BenchmarkId::new("zcash_serialize_to_vec", "BLOCK_TESTNET_141042"),
        &block,
        |b, block| b.iter(|| block.zcash_serialize_to_vec().unwrap()),
    );
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.05);
    targets = block_serialization
);
criterion_main!(benches);
