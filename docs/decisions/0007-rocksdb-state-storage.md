---
status: accepted
date: 2020-06-15
story: https://github.com/ZcashFoundation/zebra/issues/1)
---

# RocksDB for State Storage

## Context and Problem Statement

Zebra needs persistent storage for blockchain state including blocks, transactions, UTXO set, nullifier set, and note commitment trees. The storage must handle high write throughput during sync and support efficient random reads for verification.

## Priorities & Constraints

- High write throughput for initial blockchain sync
- Efficient random reads for UTXO and nullifier lookups
- Crash recovery without data corruption
- Reasonable disk space usage with compression
- Cross-platform support (Linux, macOS, Windows)
- Mature, well-tested codebase

## Considered Options

- Option 1: RocksDB
- Option 2: LMDB (Lightning Memory-Mapped Database)
- Option 3: SQLite
- Option 4: Custom storage engine

### Pros and Cons of the Options

#### Option 1: RocksDB

- Good, because it's designed for high write throughput (LSM tree)
- Good, because it's battle-tested at Facebook scale
- Good, because it supports efficient prefix iteration
- Good, because built-in compression (LZ4, Snappy, Zstd)
- Good, because good Rust bindings exist (rust-rocksdb)
- Bad, because write amplification from LSM compaction
- Bad, because tuning can be complex

#### Option 2: LMDB

- Good, because it's very fast for reads
- Good, because simple configuration
- Bad, because write performance limited by B+ tree structure
- Bad, because requires careful memory mapping management
- Bad, because database size must be pre-allocated on some platforms

#### Option 3: SQLite

- Good, because it's extremely well-tested
- Good, because SQL queries are flexible
- Bad, because not optimized for key-value workloads
- Bad, because write performance for high throughput is limited

#### Option 4: Custom Storage

- Good, because optimized for our exact use case
- Bad, because significant development and maintenance burden
- Bad, because high risk of bugs in critical component

## Decision Outcome

Chosen option: **Option 1: RocksDB**

RocksDB's LSM tree architecture is well-suited for blockchain sync, where we write many blocks sequentially. Its compression support helps manage disk usage, and the mature codebase reduces risk.

Key configuration choices:

- Column families for logical data separation
- LZ4 compression for balance of speed and ratio
- Tuned write buffer sizes for sync performance

### Expected Consequences

- Fast initial sync due to optimized sequential writes
- Column families separate blocks, transactions, UTXO set, nullifiers
- Database migrations handle format changes between versions
- Disk usage is reasonable with compression
- Cross-platform support via rust-rocksdb bindings

## More Information

- [RocksDB Documentation](https://rocksdb.org/)
- [rust-rocksdb crate](https://crates.io/crates/rocksdb)
- [State Database Upgrades Guide](https://zebra.zfnd.org/dev/state-db-upgrades.html)
