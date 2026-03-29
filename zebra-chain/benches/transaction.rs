//! Benchmarks for per-version transaction deserialization and serialization.
//!
//! Zcash has five transaction versions with increasingly complex structures:
//! - V1: transparent only (pre-Overwinter)
//! - V2: adds Sprout JoinSplits with BCTV14 proofs
//! - V3: Overwinter (adds expiry_height, version group ID)
//! - V4: Sapling (adds shielded spends/outputs with Groth16 proofs, non-sequential field order)
//! - V5: NU5 (adds Orchard actions with Halo2 proofs, different field order than V4)
//!
//! V4 deserialization is notably more complex than earlier versions because the
//! binding signature is at the end of the transaction, requiring non-sequential
//! parsing. V5 introduces yet another field ordering and Orchard support.
//!
//! # Test data
//!
//! Transactions are extracted from real mainnet blocks in `zebra-test` vectors.
//! Each version is represented by one or more transactions from blocks at the
//! appropriate network upgrade heights. The benchmark serializes each transaction
//! to bytes first, then benchmarks both deserialization and serialization.

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::io::Cursor;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use zebra_chain::{
    block::Block,
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::Transaction,
};

/// Extracts the first transaction matching a given version from a block.
fn first_tx_of_version(block: &Block, version: u32) -> Option<Vec<u8>> {
    block
        .transactions
        .iter()
        .find(|tx| tx.version() == version)
        .map(|tx| tx.zcash_serialize_to_vec().expect("valid transaction"))
}

fn bench_transaction_deserialize(c: &mut Criterion) {
    let mut group = c.benchmark_group("Transaction Deserialization");
    group.noise_threshold(0.05).sample_size(50);

    // Collect (label, serialized_tx_bytes) pairs for each version.
    let mut tx_samples: Vec<(&str, Vec<u8>)> = Vec::new();

    // V1 — transparent coinbase from genesis-era block.
    let block = Block::zcash_deserialize(Cursor::new(
        zebra_test::vectors::BLOCK_MAINNET_1_BYTES.as_slice(),
    ))
    .expect("valid block");
    if let Some(bytes) = first_tx_of_version(&block, 1) {
        tx_samples.push(("V1 transparent", bytes));
    }

    // V2 — first block with a Sprout JoinSplit (BCTV14 proofs).
    let block = Block::zcash_deserialize(Cursor::new(
        zebra_test::vectors::BLOCK_MAINNET_396_BYTES.as_slice(),
    ))
    .expect("valid block");
    if let Some(bytes) = first_tx_of_version(&block, 2) {
        tx_samples.push(("V2 sprout joinsplit", bytes));
    }

    // V3 — first Overwinter block.
    let block = Block::zcash_deserialize(Cursor::new(
        zebra_test::vectors::BLOCK_MAINNET_347500_BYTES.as_slice(),
    ))
    .expect("valid block");
    if let Some(bytes) = first_tx_of_version(&block, 3) {
        tx_samples.push(("V3 overwinter", bytes));
    }

    // V4 — Sapling block with shielded data.
    // Try 419201 first (has Sapling transactions), fall back to 419200.
    let block = Block::zcash_deserialize(Cursor::new(
        zebra_test::vectors::BLOCK_MAINNET_419201_BYTES.as_slice(),
    ))
    .expect("valid block");
    if let Some(bytes) = first_tx_of_version(&block, 4) {
        tx_samples.push(("V4 sapling", bytes));
    }

    // V5 — NU5 block with Orchard data.
    let block = Block::zcash_deserialize(Cursor::new(
        zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES.as_slice(),
    ))
    .expect("valid block");
    if let Some(bytes) = first_tx_of_version(&block, 5) {
        tx_samples.push(("V5 orchard", bytes));
    }

    for (label, tx_bytes) in &tx_samples {
        eprintln!("{}: {} bytes", label, tx_bytes.len());

        group.bench_with_input(
            BenchmarkId::new("deserialize", *label),
            tx_bytes,
            |b, bytes| {
                b.iter(|| Transaction::zcash_deserialize(Cursor::new(bytes)).unwrap())
            },
        );
    }

    group.finish();

    // Serialization benchmarks: measure the reverse direction.
    let mut group = c.benchmark_group("Transaction Serialization");
    group.noise_threshold(0.05).sample_size(50);

    for (label, tx_bytes) in &tx_samples {
        let tx = Transaction::zcash_deserialize(Cursor::new(tx_bytes)).unwrap();

        group.bench_with_input(
            BenchmarkId::new("serialize", *label),
            &tx,
            |b, tx| {
                b.iter(|| tx.zcash_serialize_to_vec().unwrap())
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_transaction_deserialize);
criterion_main!(benches);
