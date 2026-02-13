// Disabled due to warnings in criterion macros
#![allow(missing_docs)]

use std::io::Cursor;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use zebra_chain::{
    block::Block,
    sapling::{OutputInTransactionV4, PerSpendAnchor, Spend, ValueCommitment},
    serialization::{ZcashDeserialize, ZcashSerialize},
};
use zebra_test::vectors::{BLOCK_MAINNET_419201_BYTES, BLOCK_TESTNET_1842421_BYTES};

/// Count Sapling spends and outputs in a block.
fn count_sapling(block: &Block) -> (usize, usize) {
    let spends: usize = block
        .transactions
        .iter()
        .map(|tx| tx.sapling_spends_per_anchor().count())
        .sum();
    let outputs: usize = block
        .transactions
        .iter()
        .map(|tx| tx.sapling_outputs().count())
        .sum();
    (spends, outputs)
}

/// Benchmark deserialization of real Sapling-era blocks.
fn sapling_block_deserialization(c: &mut Criterion) {
    let block_419201_bytes: &[u8] = BLOCK_MAINNET_419201_BYTES.as_ref();
    let block_1842421_bytes: &[u8] = BLOCK_TESTNET_1842421_BYTES.as_ref();

    // Try to load the Sapling-spam block from disk (fetched from mainnet height 1723000).
    // This block is ~550KB with many Sapling spends+outputs from the 2023-2024 spam period.
    // To generate: fetch raw block hex from a block explorer and decode to binary.
    let spam_block_bytes = std::fs::read("/tmp/block_1723000.bin").ok();

    let mut blocks: Vec<(&str, Vec<u8>)> = vec![
        (
            "mainnet_419201_sapling",
            block_419201_bytes.to_vec(),
        ),
        (
            "testnet_1842421_sapling_v5",
            block_1842421_bytes.to_vec(),
        ),
    ];

    if let Some(ref spam_bytes) = spam_block_bytes {
        blocks.push(("mainnet_1723000_sapling_spam", spam_bytes.clone()));
    }

    // Log Sapling content of each block
    for (name, bytes) in &blocks {
        let block = Block::zcash_deserialize(Cursor::new(bytes)).unwrap();
        let (spends, outputs) = count_sapling(&block);
        eprintln!(
            "{}: {} bytes, {} sapling spends, {} sapling outputs",
            name,
            bytes.len(),
            spends,
            outputs
        );
    }

    for (name, bytes) in &blocks {
        c.bench_with_input(
            BenchmarkId::new("sapling_block_deserialize", *name),
            bytes,
            |b, bytes| b.iter(|| Block::zcash_deserialize(Cursor::new(bytes)).unwrap()),
        );
    }
}

/// Benchmark deserialization of individual Sapling spends and outputs.
///
/// Uses the Sapling-spam block if available, otherwise falls back to block 419201.
fn sapling_type_deserialization(c: &mut Criterion) {
    // Prefer the spam block for more Sapling data
    let (block_name, block_bytes): (&str, Vec<u8>) =
        if let Ok(bytes) = std::fs::read("/tmp/block_1723000.bin") {
            ("mainnet_1723000", bytes)
        } else {
            ("mainnet_419201", BLOCK_MAINNET_419201_BYTES.to_vec())
        };

    let block = Block::zcash_deserialize(Cursor::new(&block_bytes)).unwrap();

    // Collect serialized spends and outputs from all transactions
    let mut spend_bytes_list: Vec<Vec<u8>> = Vec::new();
    let mut output_bytes_list: Vec<Vec<u8>> = Vec::new();

    for tx in &block.transactions {
        for spend in tx.sapling_spends_per_anchor() {
            spend_bytes_list.push(spend.zcash_serialize_to_vec().unwrap());
        }
        for output in tx.sapling_outputs() {
            output_bytes_list.push(output.clone().into_v4().zcash_serialize_to_vec().unwrap());
        }
    }

    if spend_bytes_list.is_empty() && output_bytes_list.is_empty() {
        eprintln!("WARNING: No Sapling spends or outputs found in {block_name}");
        return;
    }

    eprintln!(
        "Extracted {} spends and {} outputs from {block_name}",
        spend_bytes_list.len(),
        output_bytes_list.len()
    );

    // Benchmark: deserialize all spends from this block
    if !spend_bytes_list.is_empty() {
        let bench_name = format!("sapling_spends_from_{block_name}");
        c.bench_function(&bench_name, |b| {
            b.iter(|| {
                for bytes in &spend_bytes_list {
                    let _: Spend<PerSpendAnchor> =
                        Spend::zcash_deserialize(Cursor::new(bytes)).unwrap();
                }
            })
        });
    }

    // Benchmark: deserialize all outputs from this block
    if !output_bytes_list.is_empty() {
        let bench_name = format!("sapling_outputs_from_{block_name}");
        c.bench_function(&bench_name, |b| {
            b.iter(|| {
                for bytes in &output_bytes_list {
                    let _ = OutputInTransactionV4::zcash_deserialize(Cursor::new(bytes)).unwrap();
                }
            })
        });
    }

    // Micro-benchmark: ValueCommitment deserialization x1000
    let cv_bytes = if let Some(spend_bytes) = spend_bytes_list.first() {
        spend_bytes[..32].to_vec()
    } else {
        output_bytes_list[0][..32].to_vec()
    };

    c.bench_function("value_commitment_deserialize_x1000", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                let _ = ValueCommitment::zcash_deserialize(Cursor::new(&cv_bytes)).unwrap();
            }
        })
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.05).sample_size(50);
    targets = sapling_block_deserialization, sapling_type_deserialization
);
criterion_main!(benches);
