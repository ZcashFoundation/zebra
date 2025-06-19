# Zebra Cached State Database Implementation

## Adding a Column Family

Most Zebra column families are implemented using low-level methods that allow accesses using any
type. But this is error-prone, because we can accidentally use different types to read and write
them. (Or read using different types in different methods.)

If we type the column family name out every time, a typo can lead to a panic, because the column
family doesn't exist.

Instead:
- define the name and type of each column family at the top of the implementation module,
- add a method on the database that returns that type, and
- add the column family name to the list of column families in the database
  (in the `STATE_COLUMN_FAMILIES_IN_CODE` array):

For example:
```rust
/// The name of the sapling transaction IDs result column family.
pub const SAPLING_TX_IDS: &str = "sapling_tx_ids";

/// The column families supported by the running `zebra-scan` database code.
pub const SCANNER_COLUMN_FAMILIES_IN_CODE: &[&str] = &[
    sapling::SAPLING_TX_IDS,
];

/// The type for reading sapling transaction IDs results from the database.
pub type SaplingTxIdsCf<'cf> =
    TypedColumnFamily<'cf, SaplingScannedDatabaseIndex, Option<SaplingScannedResult>>;

impl Storage {
    /// Returns a typed handle to the `sapling_tx_ids` column family.
    pub(crate) fn sapling_tx_ids_cf(&self) -> SaplingTxIdsCf {
        SaplingTxIdsCf::new(&self.db, SAPLING_TX_IDS)
            .expect("column family was created when database was created")
    }
}
```

Then, every read of the column family uses that method, which enforces the correct types:
(These methods have the same name as the low-level methods, but are easier to call.)
```rust
impl Storage {
    /// Returns the result for a specific database index (key, block height, transaction index).
    pub fn sapling_result_for_index(
        &self,
        index: &SaplingScannedDatabaseIndex,
    ) -> Option<SaplingScannedResult> {
        self.sapling_tx_ids_cf().zs_get(index).flatten()
    }

    /// Returns the Sapling indexes and results in the supplied range.
    fn sapling_results_in_range(
        &self,
        range: impl RangeBounds<SaplingScannedDatabaseIndex>,
    ) -> BTreeMap<SaplingScannedDatabaseIndex, Option<SaplingScannedResult>> {
        self.sapling_tx_ids_cf().zs_items_in_range_ordered(range)
    }
}
```

This simplifies the implementation compared with the raw `ReadDisk` methods.

To write to the database, use the `new_batch_for_writing()` method on the column family type.
This returns a batch that enforces the correct types. Use `write_batch()` to write it to the
database:
```rust
impl Storage {
    /// Insert a sapling scanning `key`, and mark all heights before `birthday_height` so they
    /// won't be scanned.
    pub(crate) fn insert_sapling_key(
        &mut self,
        storage: &Storage,
        sapling_key: &SaplingScanningKey,
        birthday_height: Option<Height>,
    ) {
        ...
        self.sapling_tx_ids_cf()
            .new_batch_for_writing()
            .zs_insert(&index, &None)
            .write_batch()
            .expect("unexpected database write failure");
    }
}
```

To write to an existing batch in legacy code, use `with_batch_for_writing()` instead.
This relies on the caller to write the batch to the database:
```rust
impl DiskWriteBatch {
    /// Updates the history tree for the tip, if it is not empty.
    ///
    /// The batch must be written to the database by the caller.
    pub fn update_history_tree(&mut self, db: &ZebraDb, tree: &HistoryTree) {
        let history_tree_cf = db.history_tree_cf().with_batch_for_writing(self);

        if let Some(tree) = tree.as_ref().as_ref() {
            // The batch is modified by this method and written by the caller.
            let _ = history_tree_cf.zs_insert(&(), tree);
        }
    }
}
```

To write to a legacy batch, then write it to the database, you can use
`take_batch_for_writing(batch).write_batch()`.

During database upgrades, you might need to access the same column family using different types.
[Define a type](https://github.com/ZcashFoundation/zebra/pull/8115/files#diff-ba689ca6516946a903da62153652d91dc1bb3d0100bcf08698cb3f38ead57734R36-R53)
and [convenience method](https://github.com/ZcashFoundation/zebra/pull/8115/files#diff-ba689ca6516946a903da62153652d91dc1bb3d0100bcf08698cb3f38ead57734R69-R87)
for each legacy type, and use them during the upgrade.

Some full examples of legacy code conversions, and the typed column family implementation itself
are in [PR #8112](https://github.com/ZcashFoundation/zebra/pull/8112/files) and
[PR #8115](https://github.com/ZcashFoundation/zebra/pull/8115/files).

## Current Implementation

### Verification Modes
[verification]: #verification

Zebra's state has two verification modes:
- block hash checkpoints, and
- full verification.

This means that verification uses two different codepaths, and they must produce the same results.

By default, Zebra uses as many checkpoints as it can, because they are more secure against rollbacks
(and some other kinds of chain attacks). Then it uses full verification for the last few thousand
blocks.

When Zebra gets more checkpoints in each new release, it checks the previously verified cached
state against those checkpoints. This checks that the two codepaths produce the same results.

## Upgrading the State Database
[upgrades]: #upgrades

For most state upgrades, we want to modify the database format of the existing database. If we
change the major database version, every user needs to re-download and re-verify all the blocks,
which can take days.

### Writing Blocks to the State
[write-block]: #write-block

Blocks can be written to the database via two different code paths, and both must produce the same results:

- Upgrading a pre-existing database to the latest format
- Writing newly-synced blocks in the latest format

This code is high risk, because discovering bugs is tricky, and fixing bugs can require a full reset
and re-write of an entire column family.

Most Zebra instances will do an upgrade, because they already have a cached state, and upgrades are
faster. But we run a full sync in CI every week, because new users use that codepath. (And it is
their first experience of Zebra.)

When Zebra starts up and shuts down (and periodically in CI tests), we run checks on the state
format. This makes sure that the two codepaths produce the same state on disk.

To reduce code and testing complexity:
- when a previous Zebra version opens a newer state, the entire state is considered to have that lower version, and
- when a newer Zebra version opens an older state, each required upgrade is run on the entire state.

### In-Place Upgrade Goals
[upgrade-goals]: #upgrade-goals

Here are the goals of in-place upgrades:
- avoid a full download and rebuild of the state
- Zebra must be able to upgrade the format from previous minor or patch versions of its disk format
  (Major disk format versions are breaking changes. They create a new empty state and re-sync the whole chain.)
  - this is checked the first time CI runs on a PR with a new state version.
    After the first CI run, the cached state is marked as upgraded, so the upgrade doesn't run
    again. If CI fails on the first run, any cached states with that version should be deleted.
- the upgrade and full sync formats must be identical
  - this is partially checked by the state validity checks for each upgrade (see above)
- previous zebra versions should be able to load the new format
  - this is checked by other PRs running using the upgraded cached state, but only if a Rust PR
    runs after the new PR's CI finishes, but before it merges
- best-effort loading of older supported states by newer Zebra versions
- best-effort compatibility between newer states and older supported Zebra versions

### Design Constraints
[design]: #design

Upgrades run concurrently with state verification and RPC requests.

This means that:
- the state must be able to read the old and new formats
  - it can't panic if the data is missing
  - it can't give incorrect results, because that can affect verification or wallets
  - it can return an error
  - it can only return an `Option` if the caller handles it correctly
- full syncs and upgrades must write the same format
  - the same write method should be called from both the full sync and upgrade code,
    this helps prevent data inconsistencies
- repeated upgrades must produce a valid state format
  - if Zebra is restarted, the format upgrade will run multiple times
  - if an older Zebra version opens the state, data can be written in an older format
- the format must be valid before and after each database transaction or API call, because an
  upgrade can be cancelled at any time
  - multi-column family changes should made in database transactions
  - if you are building new column family:
    - disable state queries, then enable them once it's done, or
    - do the upgrade in an order that produces correct results
      (for example, some data is valid from genesis forward, and some from the tip backward)
  - if each database API call produces a valid format, transactions aren't needed

If there is an upgrade failure, panic and tell the user to delete their cached state and re-launch
Zebra.

#### Performance Constraints
[performance]: #performance

Some column family access patterns can lead to very poor performance.

Known performance issues include:
- using an iterator on a column family which also deletes keys
- creating large numbers of iterators
- holding an iterator for a long time

See the performance notes and bug reports in:
- https://github.com/facebook/rocksdb/wiki/Iterator#iterating-upper-bound-and-lower-bound
- https://tracker.ceph.com/issues/55324
- https://jira.mariadb.org/browse/MDEV-19670

But we need to use iterators for some operations, so our alternatives are (in preferred order):
1. Minimise the number of keys we delete, and how often we delete them
2. Avoid using iterators on column families where we delete keys
3. If we must use iterators on those column families, set read bounds to minimise the amount of deleted data that is read

Currently only UTXOs require key deletion, and only `utxo_loc_by_transparent_addr_loc` requires
deletion and iterators.

### Required Tests
[testing]: #testing

State upgrades are a high-risk change. They permanently modify the state format on production Zebra
instances. Format issues are tricky to diagnose, and require extensive testing and a new release to
fix. Deleting and rebuilding an entire column family can also be costly, taking minutes or hours the
first time a cached state is upgraded to a new Zebra release.

Some format bugs can't be fixed, and require an entire rebuild of the state. For example, deleting
or corrupting transactions or block headers.

So testing format upgrades is extremely important. Every state format upgrade should test:
- new format serializations
- new calculations or data processing
- the upgrade produces a valid format
- a full sync produces a valid format

Together, the tests should cover every code path. For example, the subtrees needed mid-block,
end-of-block, sapling, and orchard tests. They mainly used the validity checks for coverage.

Each test should be followed by a restart, a sync of 200+ blocks, and another restart. This
simulates typical user behaviour.

And ideally:
- An upgrade from the earliest supported Zebra version
  (the CI sync-past-mandatory-checkpoint tests do this on every PR)

#### Manually Triggering a Format Upgrade
[manual-upgrade]: #manual-upgrade

Zebra stores the current state minor and patch versions in a `version` file in the database
directory. This path varies based on the OS, major state version, network, and config.

For example, the default mainnet state version on Linux is at:
`~/.cache/zebra/state/v25/mainnet/version`

To upgrade a cached Zebra state from `v25.0.0` to the latest disk format, delete the version file.
To upgrade from a specific version `v25.x.y`, edit the file so it contains `x.y`.

Editing the file and running Zebra will trigger a re-upgrade over an existing state.
Re-upgrades can hide format bugs. For example, if the old code was correct, and the
new code skips blocks, the validity checks won't find that bug.

So it is better to test with a full sync, and an older cached state.

## Current State Database Format
[current]: #current

rocksdb provides a persistent, thread-safe `BTreeMap<&[u8], &[u8]>`. Each map is
a distinct "tree". Keys are sorted using lexographic order (`[u8].sorted()`) on byte strings, so
integer values should be stored using big-endian encoding (so that the lex
order on byte strings is the numeric ordering).

Note that the lex order storage allows creating 1-to-many maps using keys only.
For example, the `tx_loc_by_transparent_addr_loc` allows mapping each address
to all transactions related to it, by simply storing each transaction prefixed
with the address as the key, leaving the value empty. Since rocksdb allows
listing all keys with a given prefix, it will allow listing all transactions
related to a given address.

We use the following rocksdb column families:

| Column Family                      | Keys                   | Values                        | Changes |
| ---------------------------------- | ---------------------- | ----------------------------- | ------- |
| *Blocks*                           |                        |                               |         |
| `hash_by_height`                   | `block::Height`        | `block::Hash`                 | Create  |
| `height_by_hash`                   | `block::Hash`          | `block::Height`               | Create  |
| `block_header_by_height`           | `block::Height`        | `block::Header`               | Create  |
| *Transactions*                     |                        |                               |         |
| `tx_by_loc`                        | `TransactionLocation`  | `Transaction`                 | Create  |
| `hash_by_tx_loc`                   | `TransactionLocation`  | `transaction::Hash`           | Create  |
| `tx_loc_by_hash`                   | `transaction::Hash`    | `TransactionLocation`         | Create  |
| *Transparent*                      |                        |                               |         |
| `balance_by_transparent_addr`      | `transparent::Address` | `AddressBalanceLocation`      | Update  |
| `tx_loc_by_transparent_addr_loc`   | `AddressTransaction`   | `()`                          | Create  |
| `utxo_by_out_loc`                  | `OutputLocation`       | `transparent::Output`         | Delete  |
| `utxo_loc_by_transparent_addr_loc` | `AddressUnspentOutput` | `()`                          | Delete  |
| *Sprout*                           |                        |                               |         |
| `sprout_nullifiers`                | `sprout::Nullifier`    | `()`                          | Create  |
| `sprout_anchors`                   | `sprout::tree::Root`   | `sprout::NoteCommitmentTree`  | Create  |
| `sprout_note_commitment_tree`      | `()`                   | `sprout::NoteCommitmentTree`  | Update  |
| *Sapling*                          |                        |                               |         |
| `sapling_nullifiers`               | `sapling::Nullifier`   | `()`                          | Create  |
| `sapling_anchors`                  | `sapling::tree::Root`  | `()`                          | Create  |
| `sapling_note_commitment_tree`     | `block::Height`        | `sapling::NoteCommitmentTree` | Create  |
| `sapling_note_commitment_subtree`  | `block::Height`        | `NoteCommitmentSubtreeData`   | Create  |
| *Orchard*                          |                        |                               |         |
| `orchard_nullifiers`               | `orchard::Nullifier`   | `()`                          | Create  |
| `orchard_anchors`                  | `orchard::tree::Root`  | `()`                          | Create  |
| `orchard_note_commitment_tree`     | `block::Height`        | `orchard::NoteCommitmentTree` | Create  |
| `orchard_note_commitment_subtree`  | `block::Height`        | `NoteCommitmentSubtreeData`   | Create  |
| *Chain*                            |                        |                               |         |
| `history_tree`                     | `()`                   | `NonEmptyHistoryTree`         | Update  |
| `tip_chain_value_pool`             | `()`                   | `ValueBalance`                | Update  |
| `block_info`                       | `block::Height`        | `BlockInfo`                   | Create  |

With the following additional modifications when compiled with the `indexer` feature:

| Column Family                      | Keys                   | Values                        | Changes |
| ---------------------------------- | ---------------------- | ----------------------------- | ------- |
| *Transparent*                      |                        |                               |         |
| `tx_loc_by_spent_out_loc`          | `OutputLocation`       | `TransactionLocation`         | Create  |
| *Sprout*                           |                        |                               |         |
| `sprout_nullifiers`                | `sprout::Nullifier`    | `TransactionLocation`         | Create  |
| *Sapling*                          |                        |                               |         |
| `sapling_nullifiers`               | `sapling::Nullifier`   | `TransactionLocation`         | Create  |
| *Orchard*                          |                        |                               |         |
| `orchard_nullifiers`               | `orchard::Nullifier`   | `TransactionLocation`         | Create  |

### Data Formats
[rocksdb-data-format]: #rocksdb-data-format

We use big-endian encoding for keys, to allow database index prefix searches.

Most Zcash protocol structures are encoded using `ZcashSerialize`/`ZcashDeserialize`.
Other structures are encoded using custom `IntoDisk`/`FromDisk` implementations.

Block and Transaction Data:
- `Height`: 24 bits, big-endian, unsigned (allows for ~30 years worth of blocks)
- `TransactionIndex`: 16 bits, big-endian, unsigned (max ~23,000 transactions in the 2 MB block limit)
- `TransactionCount`: same as `TransactionIndex`
- `TransactionLocation`: `Height \|\| TransactionIndex`
- `AddressBalanceLocation`: `Amount \|\| u64 \|\| AddressLocation`
- `OutputIndex`: 24 bits, big-endian, unsigned (max ~223,000 transfers in the 2 MB block limit)
- transparent and shielded input indexes, and shielded output indexes: 16 bits, big-endian, unsigned (max ~49,000 transfers in the 2 MB block limit)
- `OutputLocation`: `TransactionLocation \|\| OutputIndex`
- `AddressLocation`: the first `OutputLocation` used by a `transparent::Address`.
  Always has the same value for each address, even if the first output is spent.
- `Utxo`: `Output`, derives extra fields from the `OutputLocation` key
- `AddressUnspentOutput`: `AddressLocation \|\| OutputLocation`,
  used instead of a `BTreeSet<OutputLocation>` value, to improve database performance
- `AddressTransaction`: `AddressLocation \|\| TransactionLocation`
  used instead of a `BTreeSet<TransactionLocation>` value, to improve database performance
- `NoteCommitmentSubtreeIndex`: 16 bits, big-endian, unsigned
- `NoteCommitmentSubtreeData<{sapling, orchard}::tree::Node>`: `Height \|\| {sapling, orchard}::tree::Node`

Amounts:
- `Amount`: 64 bits, little-endian, signed
- `ValueBalance`: `[Amount; 4]`

Derived Formats (legacy):
- `*::NoteCommitmentTree`: `bincode` using `serde`
  - stored note commitment trees always have cached roots
- `NonEmptyHistoryTree`: `bincode` using `serde`, using our copy of an old `zcash_history` `serde`
  implementation

`bincode` is a risky format to use, because it depends on the exact order and type of struct fields.
Do not use it for new column families.

#### Address Format
[rocksdb-address-format]: #rocksdb-address-format

The following figure helps visualizing the address index, which is the most complicated part.
Numbers in brackets are array sizes; bold arrows are compositions (i.e. `TransactionLocation` is the
concatenation of `Height` and `TransactionIndex`); dashed arrows are compositions that are also 1-to-many
maps (i.e. `AddressTransaction` is the concatenation of `AddressLocation` and `TransactionLocation`,
but also is used to map each `AddressLocation` to multiple `TransactionLocation`s).

```mermaid
graph TD;
    Address -->|"balance_by_transparent_addr<br/>"| AddressBalance;
    AddressBalance ==> Amount;
    AddressBalance ==> AddressLocation;
    AddressLocation ==> FirstOutputLocation;
    AddressLocation -.->|"tx_loc_by_transparent_addr_loc<br/>(AddressTransaction[13])"| TransactionLocation;
    TransactionLocation ==> Height;
    TransactionLocation ==> TransactionIndex;
    OutputLocation -->|utxo_by_out_loc| Output;
    OutputLocation ==> TransactionLocation;
    OutputLocation ==> OutputIndex;
    AddressLocation -.->|"utxo_loc_by_transparent_addr_loc<br/>(AddressUnspentOutput[16])"| OutputLocation;

    AddressBalance["AddressBalance[16]"];
    Amount["Amount[8]"];
    Height["Height[3]"];
    Address["Address[21]"];
    TransactionIndex["TransactionIndex[2]"];
    TransactionLocation["TransactionLocation[5]"];
    OutputIndex["OutputIndex[3]"];
    OutputLocation["OutputLocation[8]"];
    FirstOutputLocation["First OutputLocation[8]"];
    AddressLocation["AddressLocation[8]"];
```

### Implementing consensus rules using rocksdb
[rocksdb-consensus-rules]: #rocksdb-consensus-rules

Each column family handles updates differently, based on its specific consensus rules:
- Create:
  - Each key-value entry is created once.
  - Keys are never deleted, values are never updated.
- Delete:
  - Each key-value entry is created once.
  - Keys can be deleted, but values are never updated.
  - Code called by ReadStateService must ignore deleted keys, or use a read lock.
  - We avoid deleting keys, and avoid using iterators on `Delete` column families, for performance.
  - TODO: should we prevent re-inserts of keys that have been deleted?
- Update:
  - Each key-value entry is created once.
  - Keys are never deleted, but values can be updated.
  - Code called by ReadStateService must handle old or new values, or use a read lock.

We can't do some kinds of value updates, because they cause RocksDB performance issues:
- Append:
  - Keys are never deleted.
  - Existing values are never updated.
  - Sets of values have additional items appended to the end of the set.
  - Code called by ReadStateService must handle shorter or longer sets, or use a read lock.
- Up/Del:
  - Keys can be deleted.
  - Sets of values have items added or deleted (in any position).
  - Code called by ReadStateService must ignore deleted keys and values,
    accept shorter or longer sets, and accept old or new values.
    Or it should use a read lock.

Avoid using large sets of values as RocksDB keys or values.

### RocksDB read locks
[rocksdb-read-locks]: #rocksdb-read-locks

The read-only ReadStateService needs to handle concurrent writes and deletes of the finalized
column families it reads. It must also handle overlaps between the cached non-finalized `Chain`,
and the current finalized state database.

The StateService uses RocksDB transactions for each block write.
So ReadStateService queries that only access a single key or value will always see
a consistent view of the database.

If a ReadStateService query only uses column families that have keys and values appended
(`Never` in the Updates table above), it should ignore extra appended values.
Most queries do this by default.

For more complex queries, there are several options:

Reading across multiple column families:
1. Ignore deleted values using custom Rust code
2. Take a database snapshot - https://docs.rs/rocksdb/latest/rocksdb/struct.DBWithThreadMode.html#method.snapshot

Reading a single column family:
3. multi_get - https://docs.rs/rocksdb/latest/rocksdb/struct.DBWithThreadMode.html#method.multi_get_cf
4. iterator - https://docs.rs/rocksdb/latest/rocksdb/struct.DBWithThreadMode.html#method.iterator_cf

RocksDB also has read transactions, but they don't seem to be exposed in the Rust crate.

### Low-Level Implementation Details
[rocksdb-low-level]: #rocksdb-low-level

RocksDB ignores duplicate puts and deletes, preserving the latest values.
If rejecting duplicate puts or deletes is consensus-critical,
check [`db.get_cf(cf, key)?`](https://docs.rs/rocksdb/0.16.0/rocksdb/struct.DBWithThreadMode.html#method.get_cf)
before putting or deleting any values in a batch.

Currently, these restrictions should be enforced by code review:
- multiple `zs_insert`s are only allowed on Update column families, and
- [`delete_cf`](https://docs.rs/rocksdb/0.16.0/rocksdb/struct.WriteBatch.html#method.delete_cf)
  is only allowed on Delete column families.

In future, we could enforce these restrictions by:
- creating traits for Never, Delete, and Update
- doing different checks in `zs_insert` depending on the trait
- wrapping `delete_cf` in a trait, and only implementing that trait for types that use Delete column families.

As of June 2021, the Rust `rocksdb` crate [ignores the delete callback](https://docs.rs/rocksdb/0.16.0/src/rocksdb/merge_operator.rs.html#83-94),
and merge operators are unreliable (or have undocumented behaviour).
So they should not be used for consensus-critical checks.

### Notes on rocksdb column families
[rocksdb-column-families]: #rocksdb-column-families

- The `hash_by_height` and `height_by_hash` column families provide a bijection between
  block heights and block hashes.  (Since the rocksdb state only stores finalized
  state, they are actually a bijection).

- Similarly, the `tx_loc_by_hash` and `hash_by_tx_loc` column families provide a bijection between
  transaction locations and transaction hashes.

- The `block_header_by_height` column family provides a bijection between block
  heights and block header data. There is no corresponding `height_by_block` column
  family: instead, hash the block header, and use the hash from `height_by_hash`.
  (Since the rocksdb state only stores finalized state, they are actually a bijection).
  Similarly, there are no column families that go from transaction data
  to transaction locations: hash the transaction and use `tx_loc_by_hash`.

- Block headers and transactions are stored separately in the database,
  so that individual transactions can be accessed efficiently.
  Blocks can be re-created on request using the following process:
  - Look up `height` in `height_by_hash`
  - Get the block header for `height` from `block_header_by_height`
  - Iterate from `TransactionIndex` 0,
    to get each transaction with `height` from `tx_by_loc`,
    stopping when there are no more transactions in the block

- Block headers are stored by height, not by hash.  This has the downside that looking
  up a block by hash requires an extra level of indirection.  The upside is
  that blocks with adjacent heights are adjacent in the database, and many
  common access patterns, such as helping a client sync the chain or doing
  analysis, access blocks in (potentially sparse) height order.  In addition,
  the fact that we commit blocks in order means we're writing only to the end
  of the rocksdb column family, which may help save space.

- Similarly, transaction data is stored in chain order in `tx_by_loc` and `utxo_by_out_loc`,
  and chain order within each vector in `utxo_loc_by_transparent_addr_loc` and
  `tx_loc_by_transparent_addr_loc`.

- `TransactionLocation`s are stored as a `(height, index)` pair referencing the
  height of the transaction's parent block and the transaction's index in that
  block.  This would more traditionally be a `(hash, index)` pair, but because
  we store blocks by height, storing the height saves one level of indirection.
  Transaction hashes can be looked up using `hash_by_tx_loc`.

- Similarly, UTXOs are stored in `utxo_by_out_loc` by `OutputLocation`,
  rather than `OutPoint`. `OutPoint`s can be looked up using `tx_loc_by_hash`,
  and reconstructed using `hash_by_tx_loc`.

- The `Utxo` type can be constructed from the `OutputLocation` and `Output` data,
  `height: OutputLocation.height`, and
  `is_coinbase: OutputLocation.transaction_index == 0`
  (coinbase transactions are always the first transaction in a block).

- `balance_by_transparent_addr` is the sum of all `utxo_loc_by_transparent_addr_loc`s
  that are still in `utxo_by_out_loc`. It is cached to improve performance for
  addresses with large UTXO sets. It also stores the `AddressLocation` for each
  address, which allows for efficient lookups.

- `utxo_loc_by_transparent_addr_loc` stores unspent transparent output locations
  by address. The address location and UTXO location are stored as a RocksDB key,
  so they are in chain order, and get good database performance.
  This column family includes also includes the original address location UTXO,
  if it has not been spent.

- When a block write deletes a UTXO from `utxo_by_out_loc`,
  that UTXO location should be deleted from `utxo_loc_by_transparent_addr_loc`.
  The deleted UTXO can be removed efficiently, because the UTXO location is part of the key.
  This is an index optimisation, which does not affect query results.

- `tx_loc_by_transparent_addr_loc` stores transaction locations by address.
  This list includes transactions containing spent UTXOs.
  The address location and transaction location are stored as a RocksDB key,
  so they are in chain order, and get good database performance.
  This column family also includes the `TransactionLocation`
  of the transaction for the `AddressLocation`.

- The `sprout_note_commitment_tree` stores the note commitment tree state
  at the tip of the finalized state, for the specific pool. There is always
  a single entry. Each tree is stored
  as a "Merkle tree frontier" which is basically a (logarithmic) subset of
  the Merkle tree nodes as required to insert new items.
  For each block committed, the old tree is deleted and a new one is inserted
  by its new height.

- The `{sapling, orchard}_note_commitment_tree` stores the note commitment tree
  state for every height, for the specific pool. Each tree is stored
  as a "Merkle tree frontier" which is basically a (logarithmic) subset of
  the Merkle tree nodes as required to insert new items.

- The `{sapling, orchard}_note_commitment_subtree` stores the completion height and
  root for every completed level 16 note commitment subtree, for the specific pool.

- `history_tree` stores the ZIP-221 history tree state at the tip of the finalized
  state. There is always a single entry for it. The tree is stored as the set of "peaks"
  of the "Merkle mountain range" tree structure, which is what is required to
  insert new items.

- Each `*_anchors` stores the anchor (the root of a Merkle tree) of the note commitment
  tree of a certain block. We only use the keys since we just need the set of anchors,
  regardless of where they come from. The exception is `sprout_anchors` which also maps
  the anchor to the matching note commitment tree. This is required to support interstitial
  treestates, which are unique to Sprout.
  **TODO:** store the `Root` hash in `sprout_note_commitment_tree`, and use it to look up the
  note commitment tree. This de-duplicates tree state data. But we currently only store one sprout tree by height.

- The value pools are only stored for the finalized tip. Per-block value pools
  are stored in `block_info`, see below.

- We do not store the cumulative work for the finalized chain,
  because the finalized work is equal for all non-finalized chains.
  So the additional non-finalized work can be used to calculate the relative chain order,
  and choose the best chain.

- The `block_info` contains additional per-block data. Currently it stores
  the value pools after that block, and its size. It has been implemented
  in a future-proof way so it is possible to add more data to it whiles
  still allowing database downgrades (i.e. it does not require the
  data length to match exactly what is expected and ignores the rest)
