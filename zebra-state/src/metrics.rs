//! Descriptions of the metrics this crate records.
//!
//! Applications that install a [`metrics`] recorder call [`describe`] right after installing it, so
//! that exporters can document each metric, for example in Prometheus `# HELP` lines.

use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};

/// The label keys of each labelled metric this crate records.
///
/// - `state.requests`:
///   - `service`: `state` or `read_state`.
///   - `type`: the request variant name, from `Request::variant_name` or
///     `ReadRequest::variant_name`.
/// - `zcash.pool.value.zatoshis` and `zcash.pool.notes.created`:
///   - `name`: a pool, one of `transparent` (value only), `sprout`, `sapling`, `orchard`, or
///     `ironwood`.
/// - `zebra.state.rocksdb.cf_*`:
///   - `cf`: a RocksDB column family name.
/// - `zebra.state.rocksdb.num_files_at_level`:
///   - `level`: a RocksDB level, `0` to `6`.
pub const LABELS: &[(&str, &[&str])] = &[
    ("state.requests", &["service", "type"]),
    ("zcash.pool.value.zatoshis", &["name"]),
    ("zcash.pool.notes.created", &["name"]),
    ("zebra.state.rocksdb.cf_disk_size_bytes", &["cf"]),
    ("zebra.state.rocksdb.cf_memory_size_bytes", &["cf"]),
    ("zebra.state.rocksdb.num_files_at_level", &["level"]),
];

/// Describes every metric this crate records to the installed [`metrics`] recorder.
pub fn describe() {
    describe_counter!(
        "state.requests",
        Unit::Count,
        "Requests to the state and read state services, by request type."
    );

    // Chain tip
    describe_gauge!(
        "zcash.chain.verified.block.height",
        Unit::Count,
        "Height of the latest block committed to the state, by either verifier."
    );
    describe_counter!(
        "zcash.chain.verified.block.total",
        Unit::Count,
        "Blocks committed to the state, by either verifier."
    );

    // Checkpoint-verified blocks
    describe_gauge!(
        "state.checkpoint.queued.max.height",
        Unit::Count,
        "Highest checkpoint-verified block height queued for the finalized state, or NaN when none are queued."
    );
    describe_gauge!(
        "state.checkpoint.queued.block.count",
        Unit::Count,
        "Checkpoint-verified blocks queued for the finalized state."
    );
    describe_gauge!(
        "state.checkpoint.sent.block.height",
        Unit::Count,
        "Height of the latest checkpoint-verified block sent to the block write task."
    );
    describe_counter!(
        "state.checkpoint.finalized.block.count",
        Unit::Count,
        "Checkpoint-verified blocks committed to the finalized state."
    );
    describe_gauge!(
        "state.checkpoint.finalized.block.height",
        Unit::Count,
        "Height of the latest checkpoint-verified block committed to the finalized state."
    );
    describe_counter!(
        "state.checkpoint.error.block.count",
        Unit::Count,
        "Checkpoint-verified blocks that failed to commit to the finalized state."
    );
    describe_gauge!(
        "state.checkpoint.error.block.height",
        Unit::Count,
        "Height of the latest checkpoint-verified block that failed to commit."
    );

    // Fully verified blocks
    describe_counter!(
        "state.full_verifier.committed.block.count",
        Unit::Count,
        "Fully verified blocks committed to the state."
    );
    describe_gauge!(
        "state.full_verifier.committed.block.height",
        Unit::Count,
        "Height of the latest fully verified block committed to the state."
    );

    // Non-finalized state
    describe_counter!(
        "state.memory.committed.block.count",
        Unit::Count,
        "Blocks committed to any chain in the non-finalized state."
    );
    describe_gauge!(
        "state.memory.committed.block.height",
        Unit::Count,
        "Height of the latest block committed to any chain in the non-finalized state."
    );
    describe_counter!(
        "state.memory.best.committed.block.count",
        Unit::Count,
        "Blocks committed to the best chain in the non-finalized state."
    );
    describe_gauge!(
        "state.memory.best.committed.block.height",
        Unit::Count,
        "Height of the latest block committed to the best chain in the non-finalized state."
    );
    describe_gauge!(
        "state.memory.chain.count",
        Unit::Count,
        "Chains in the non-finalized state."
    );
    describe_gauge!(
        "state.memory.best.chain.length",
        Unit::Count,
        "Blocks in the best chain of the non-finalized state."
    );
    describe_gauge!(
        "zcash.pool.value.zatoshis",
        Unit::Count,
        "Value in each pool at the best chain tip, in zatoshis."
    );
    describe_gauge!(
        "zcash.pool.notes.created",
        Unit::Count,
        "Note commitments in each shielded pool's tree at the best chain tip."
    );

    // Queued fully verified blocks
    describe_gauge!(
        "state.memory.queued.min.height",
        Unit::Count,
        "Lowest height of fully verified blocks waiting for their parent, or NaN when none are queued."
    );
    describe_gauge!(
        "state.memory.queued.max.height",
        Unit::Count,
        "Highest height of fully verified blocks waiting for their parent, or NaN when none are queued."
    );
    describe_gauge!(
        "state.memory.queued.block.count",
        Unit::Count,
        "Fully verified blocks waiting for their parent."
    );
    describe_counter!(
        "state.memory.sent.block.count",
        Unit::Count,
        "Fully verified blocks sent to the block write task."
    );
    describe_gauge!(
        "state.memory.sent.block.height",
        Unit::Count,
        "Height of the latest fully verified block sent to the block write task."
    );
    describe_gauge!(
        "state.memory.sent.cache.min.height",
        Unit::Count,
        "Lowest height in the cache of sent blocks, or NaN when it is empty."
    );
    describe_gauge!(
        "state.memory.sent.cache.max.height",
        Unit::Count,
        "Highest height in the cache of sent blocks, or NaN when it is empty."
    );
    describe_gauge!(
        "state.memory.sent.cache.block.count",
        Unit::Count,
        "Blocks in the cache of sent blocks."
    );
    describe_gauge!(
        "state.memory.sent.cache.batch.count",
        Unit::Count,
        "Batches in the cache of sent blocks."
    );

    // Finalized state
    describe_counter!(
        "state.finalized.block.count",
        Unit::Count,
        "Blocks committed to the finalized state."
    );
    describe_gauge!(
        "state.finalized.block.height",
        Unit::Count,
        "Height of the finalized tip."
    );
    describe_counter!(
        "state.finalized.cumulative.transactions",
        Unit::Count,
        "Transactions in blocks committed to the finalized state."
    );
    describe_counter!(
        "state.finalized.cumulative.sprout_nullifiers",
        Unit::Count,
        "Sprout nullifiers revealed by blocks committed to the finalized state."
    );
    describe_counter!(
        "state.finalized.cumulative.sapling_nullifiers",
        Unit::Count,
        "Sapling nullifiers revealed by blocks committed to the finalized state."
    );
    describe_counter!(
        "state.finalized.cumulative.orchard_nullifiers",
        Unit::Count,
        "Orchard nullifiers revealed by blocks committed to the finalized state."
    );
    describe_counter!(
        "state.finalized.cumulative.transparent_prevouts",
        Unit::Count,
        "Transparent outputs spent by blocks committed to the finalized state, after genesis."
    );
    describe_counter!(
        "state.finalized.cumulative.transparent_newouts",
        Unit::Count,
        "Transparent outputs created by blocks committed to the finalized state, after genesis."
    );
    describe_gauge!(
        "state.finalized.value_pool.transparent",
        Unit::Count,
        "Transparent pool balance at the finalized tip, in zatoshis."
    );
    describe_gauge!(
        "state.finalized.value_pool.sprout",
        Unit::Count,
        "Sprout pool balance at the finalized tip, in zatoshis."
    );
    describe_gauge!(
        "state.finalized.value_pool.sapling",
        Unit::Count,
        "Sapling pool balance at the finalized tip, in zatoshis."
    );
    describe_gauge!(
        "state.finalized.value_pool.orchard",
        Unit::Count,
        "Orchard pool balance at the finalized tip, in zatoshis."
    );
    describe_gauge!(
        "state.finalized.value_pool.ironwood",
        Unit::Count,
        "Ironwood pool balance at the finalized tip, in zatoshis."
    );
    describe_gauge!(
        "state.finalized.value_pool.deferred",
        Unit::Count,
        "Deferred lockbox pool balance at the finalized tip, in zatoshis."
    );
    describe_gauge!(
        "state.finalized.chain_supply.total",
        Unit::Count,
        "Total value in all pools at the finalized tip, in zatoshis."
    );

    // RocksDB
    describe_histogram!(
        "zebra.state.rocksdb.batch_commit.duration_seconds",
        Unit::Seconds,
        "Time taken to write one block's batch to RocksDB."
    );
    describe_gauge!(
        "zebra.state.rocksdb.cf_disk_size_bytes",
        Unit::Bytes,
        "Disk space used by each RocksDB column family."
    );
    describe_gauge!(
        "zebra.state.rocksdb.cf_memory_size_bytes",
        Unit::Bytes,
        "Memory used by each RocksDB column family's memtables."
    );
    describe_gauge!(
        "zebra.state.rocksdb.total_disk_size_bytes",
        Unit::Bytes,
        "Disk space used by all RocksDB column families."
    );
    describe_gauge!(
        "zebra.state.rocksdb.live_data_size_bytes",
        Unit::Bytes,
        "Estimated size of the live data in all RocksDB column families."
    );
    describe_gauge!(
        "zebra.state.rocksdb.total_memory_size_bytes",
        Unit::Bytes,
        "Memory used by all RocksDB memtables."
    );
    describe_gauge!(
        "zebra.state.rocksdb.compaction.pending_bytes",
        Unit::Count,
        "1 when RocksDB has at least one compaction pending, otherwise 0."
    );
    describe_gauge!(
        "zebra.state.rocksdb.compaction.running",
        Unit::Count,
        "RocksDB compactions currently running."
    );
    describe_gauge!(
        "zebra.state.rocksdb.block_cache_usage_bytes",
        Unit::Bytes,
        "Memory used by the RocksDB block cache."
    );
    describe_gauge!(
        "zebra.state.rocksdb.num_files_at_level",
        Unit::Count,
        "RocksDB files at each level."
    );
}
