//! Benchmarks for the `getblock` response hot paths.
//!
//! - Verbosity 0: the RPC deserializes the stored block, then re-serializes and hex-encodes it.
//!   `raw` compares that with hex-encoding the stored bytes directly.
//! - State reads: `ReadRequest::Block` + serialize vs `ReadRequest::RawBlock` through a
//!   `ReadStateService` with the continuous mainnet vectors finalized, and the same for transactions.
//! - Verbosity 2: the RPC builds a `TransactionObject` for every transaction, then serializes
//!   it to JSON. `verbose` measures the whole step and its per-transaction parts.
//! - `getblock rpc`: end-to-end `get_block` latency through `RpcImpl` against a populated state,
//!   including every state read the method issues.
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
use tower::{buffer::Buffer, ServiceExt};

use zebra_chain::{
    block::{Block, SerializedBlock},
    chain_sync_status::MockSyncStatus,
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::SerializedTransaction,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::BoxError;
use zebra_rpc::{
    client::{GetBlockResponse, TransactionObject},
    methods::{RpcImpl, RpcServer},
};
use zebra_test::mock_service::MockService;

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

fn bench_state(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime builds");
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| Arc::new(Block::zcash_deserialize(Cursor::new(*bytes)).expect("valid block")))
        .collect();
    let (_state, read_state, _tip, _tip_change) = runtime.block_on(zebra_state::populated_state(
        blocks.clone(),
        &Network::Mainnet,
    ));

    let read = |request| {
        runtime
            .block_on(read_state.clone().oneshot(request))
            .expect("state read succeeds")
    };

    let mut group = c.benchmark_group("getblock verbosity 0 state");
    // Genesis transactions are not indexed, so skip it.
    for block in blocks.iter().skip(1).step_by(4) {
        let height = block.coinbase_height().expect("valid coinbase height");
        let txid = block.transactions[0].hash();
        let label = format!(
            "{} ({} tx, {} B)",
            height.0,
            block.transactions.len(),
            block.zcash_serialized_size()
        );

        group.bench_function(BenchmarkId::new("block: Block + serialize", &label), |b| {
            b.iter(
                || match read(zebra_state::ReadRequest::Block(height.into())) {
                    zebra_state::ReadResponse::Block(Some(block)) => SerializedBlock::from(block),
                    _ => unreachable!("block is in the state"),
                },
            )
        });
        group.bench_function(BenchmarkId::new("block: RawBlock", &label), |b| {
            b.iter(
                || match read(zebra_state::ReadRequest::RawBlock(height.into())) {
                    zebra_state::ReadResponse::RawBlock(Some(block)) => block,
                    _ => unreachable!("block is in the state"),
                },
            )
        });
        group.bench_function(
            BenchmarkId::new("tx: AnyChainTransaction + serialize", &label),
            |b| {
                b.iter(
                    || match read(zebra_state::ReadRequest::AnyChainTransaction(txid)) {
                        zebra_state::ReadResponse::AnyChainTransaction(Some(tx)) => {
                            let tx: Arc<zebra_chain::transaction::Transaction> = tx.into();
                            SerializedTransaction::from(tx)
                        }
                        _ => unreachable!("transaction is in the state"),
                    },
                )
            },
        );
        group.bench_function(
            BenchmarkId::new("tx: AnyChainRawTransaction", &label),
            |b| {
                b.iter(
                    || match read(zebra_state::ReadRequest::AnyChainRawTransaction(txid)) {
                        zebra_state::ReadResponse::RawTransaction(Some(tx)) => tx,
                        _ => unreachable!("transaction is in the state"),
                    },
                )
            },
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

fn bench_rpc(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let _guard = rt.enter();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into().expect("valid block"))
        .collect();
    let tip = (blocks.len() - 1).to_string();
    let tip_hash = blocks.last().expect("non-empty").hash().to_string();

    let (state, read_state, latest_chain_tip, _) =
        rt.block_on(zebra_state::populated_state(blocks, &Network::Mainnet));

    // Same service wiring as the `rpc_getblock` vector test.
    let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let (rpc, _queue) = RpcImpl::new(
        Network::Mainnet,
        Default::default(),
        Default::default(),
        "0.0.1",
        "RPC bench",
        Buffer::new(mempool, 1),
        Buffer::new(state, 1),
        Buffer::new(read_state, 1),
        MockService::build().for_unit_tests(),
        MockSyncStatus::default(),
        latest_chain_tip,
        MockAddressBookPeers::default(),
        rx,
        None,
    );

    let mut group = c.benchmark_group("getblock rpc");
    for (label, id, verbosity) in [
        ("v1 by height", &tip, 1),
        ("v1 by hash", &tip_hash, 1),
        ("v2 by height", &tip, 2),
        ("v3 by height", &tip, 3),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                rt.block_on(rpc.get_block(id.clone(), Some(verbosity)))
                    .expect("block exists")
            })
        });
    }
    group.bench_function("getblockheader by height", |b| {
        b.iter(|| {
            rt.block_on(rpc.get_block_header(tip.clone(), Some(true)))
                .expect("block exists")
        })
    });
    group.finish();
}

criterion_group!(benches, bench_raw, bench_state, bench_verbose, bench_rpc);
criterion_main!(benches);
