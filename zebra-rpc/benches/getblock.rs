//! Benchmarks for the `getblock` response hot paths.
//!
//! - Verbosity 0: the RPC deserializes the stored block, then re-serializes and hex-encodes it.
//!   `raw` compares that with hex-encoding the stored bytes directly.
//! - Verbosity 2: the RPC builds a `TransactionObject` for every transaction, then serializes
//!   it to JSON. `verbose` measures the whole step and its per-transaction parts.
//!
//! # Test data
//!
//! The largest shielded mainnet blocks in the `zebra-test` vectors, plus a transparent-heavy one:
//! - 1,687,107: NU5, 6 V4/V5 txs, 2 Sapling spends, 3 Sapling outputs, 2 Orchard actions (16 KB)
//! - 1,687,118: NU5, 8 V4/V5 txs, 3 spends, 5 outputs, 2 actions, 221 transparent outputs (25 KB)
//! - 653,601: Heartwood, 9 V4 txs, 6 Sapling spends, 9 Sapling outputs (14 KB)
//! - 1,046,401: Canopy, 47 V4 txs, 422 transparent inputs, the largest vector (73 KB)

// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::{io::Cursor, sync::Arc};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use zebra_chain::{
    block::{Block, SerializedBlock},
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::SerializedTransaction,
};
use zebra_rpc::client::{GetBlockResponse, TransactionObject};

/// Returns `(label, raw bytes, parsed block)` for each benchmarked mainnet block.
fn blocks() -> Vec<(String, &'static [u8], Arc<Block>)> {
    [1_687_107, 1_687_118, 653_601, 1_046_401]
        .into_iter()
        .map(|height| {
            let bytes = *zebra_test::vectors::MAINNET_BLOCKS
                .get(&height)
                .expect("height is in the vectors");
            let block = Block::zcash_deserialize(Cursor::new(bytes)).expect("valid block");
            let label = format!(
                "{height} ({} tx, {} B)",
                block.transactions.len(),
                bytes.len()
            );
            (label, bytes, Arc::new(block))
        })
        .collect()
}

/// Builds the verbosity 2 transaction objects for `block`, like `get_block` does.
fn transaction_objects(block: &Arc<Block>) -> Vec<TransactionObject> {
    let network = Network::Mainnet;
    let height = block.coinbase_height().expect("valid coinbase height");
    let hash = block.hash();

    block
        .transactions
        .iter()
        .map(|tx| {
            TransactionObject::from_transaction(
                tx.clone(),
                Some(height),
                Some(1),
                &network,
                Some(block.header.time),
                Some(hash),
                Some(true),
                tx.hash(),
            )
        })
        .collect()
}

fn bench_raw(c: &mut Criterion) {
    let mut group = c.benchmark_group("getblock verbosity 0");

    for (label, bytes, block) in blocks() {
        group.throughput(Throughput::Bytes(bytes.len() as u64));

        // Today: the state deserializes every transaction, then the RPC re-serializes the block.
        group.bench_with_input(
            BenchmarkId::new("deserialize + serialize + hex json", &label),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    let block = Arc::new(
                        Block::zcash_deserialize(Cursor::new(*bytes)).expect("valid block"),
                    );
                    let response = GetBlockResponse::Raw(block.into());
                    serde_json::to_vec(&response).expect("valid json")
                })
            },
        );

        // Alternative: hex-encode the stored bytes directly.
        group.bench_with_input(
            BenchmarkId::new("stored bytes + hex json", &label),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    let response = GetBlockResponse::Raw(SerializedBlock::from(bytes.to_vec()));
                    serde_json::to_vec(&response).expect("valid json")
                })
            },
        );

        // The parts of the current path.
        group.bench_with_input(
            BenchmarkId::new("deserialize", &label),
            &bytes,
            |b, bytes| {
                b.iter(|| Block::zcash_deserialize(Cursor::new(*bytes)).expect("valid block"))
            },
        );
        group.bench_with_input(BenchmarkId::new("serialize", &label), &block, |b, block| {
            b.iter(|| SerializedBlock::from(block.as_ref()))
        });
        let serialized = GetBlockResponse::Raw(SerializedBlock::from(bytes.to_vec()));
        group.bench_with_input(
            BenchmarkId::new("hex json", &label),
            &serialized,
            |b, response| b.iter(|| serde_json::to_vec(response).expect("valid json")),
        );
    }

    group.finish();
}

fn bench_verbose(c: &mut Criterion) {
    let mut group = c.benchmark_group("getblock verbosity 2");

    for (label, _bytes, block) in blocks() {
        group.bench_with_input(
            BenchmarkId::new("objects + json", &label),
            &block,
            |b, block| {
                b.iter(|| {
                    let objects = transaction_objects(block);
                    serde_json::to_vec(&objects).expect("valid json")
                })
            },
        );
        group.bench_with_input(BenchmarkId::new("objects", &label), &block, |b, block| {
            b.iter(|| transaction_objects(block))
        });

        let objects = transaction_objects(&block);
        group.bench_with_input(BenchmarkId::new("json", &label), &objects, |b, objects| {
            b.iter(|| serde_json::to_vec(objects).expect("valid json"))
        });

        // Per-transaction work inside `from_transaction`, summed over the block.
        let txs = &block.transactions;
        group.bench_with_input(
            BenchmarkId::new("part: hex re-serialize", &label),
            txs,
            |b, txs| {
                b.iter(|| {
                    txs.iter()
                        .map(|tx| SerializedTransaction::from(tx.clone()))
                        .collect::<Vec<_>>()
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("part: serialized size", &label),
            txs,
            |b, txs| {
                b.iter(|| {
                    txs.iter()
                        .map(|tx| tx.zcash_serialized_size())
                        .sum::<usize>()
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("part: auth digest", &label),
            txs,
            |b, txs| b.iter(|| txs.iter().map(|tx| tx.auth_digest()).collect::<Vec<_>>()),
        );
        group.bench_with_input(BenchmarkId::new("part: txid", &label), txs, |b, txs| {
            b.iter(|| txs.iter().map(|tx| tx.hash()).collect::<Vec<_>>())
        });
        group.bench_with_input(
            BenchmarkId::new("part: inputs + outputs", &label),
            txs,
            |b, txs| {
                b.iter(|| {
                    txs.iter()
                        .map(|tx| (tx.inputs(), tx.outputs()))
                        .collect::<Vec<_>>()
                })
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_raw, bench_verbose);
criterion_main!(benches);
