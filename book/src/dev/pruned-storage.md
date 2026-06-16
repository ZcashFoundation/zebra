# Pruned Storage Mode

Zebra can run in a **pruned** storage mode that deletes historical raw
transaction data once it falls outside a configurable retention window. This
reduces on-disk state size while keeping the node fully consensus-valid: it
continues to validate and sync new blocks, including spends of UTXOs created in
blocks whose raw transaction bytes have been deleted.

This document describes the design: the two storage modes and their
configuration, how a node switches between them, what "retention height" means,
how pruning is applied during normal block commits, and the offline tool for
pruning an existing database.

## Storage modes

Storage is controlled by the `state.storage_mode` configuration field, which has
two modes:

- **`archive`** (default) — the node retains and can serve _all_ historical raw
  transaction data. This is required to answer historical RPC queries such as
  `getrawtransaction` for arbitrary past transactions.
- **`pruned`** — the node may delete historical raw transaction bytes for blocks
  older than a configured retention window. Consensus validity is preserved; only
  the ability to serve old raw transaction data is given up.

### Configuration

`storage_mode` accepts either a bare string or a table. Archive mode (the
default) needs no configuration:

```toml
[state]
storage_mode = "archive"
```

Pruned mode can be selected with a bare string, which uses the default retention
window:

```toml
[state]
storage_mode = "pruned"
```

Or as a table, to set the retention window explicitly:

```toml
[state.storage_mode.pruned]
tx_retention = 6000
```

`tx_retention` is the number of recent finalized blocks below the tip whose raw
transaction data is retained. It must be at least the network's minimum retention
floor (see [Retention height](#retention-height-and-per-block-pruning) below);
this is enforced at startup by `Config::validate_storage_mode`, which rejects an
out-of-range value before the database is opened. The default is
`MIN_PRUNING_RETENTION` (10_000 blocks on Mainnet and Testnet).

### What is pruned and what is retained

Only the raw transaction bytes (`tx_by_loc`) are pruned — and that is where
essentially all of the disk savings come from. The transaction _location_
indexes are deliberately **retained**:

| Column family | Pruned? | Why |
| --- | --- | --- |
| `tx_by_loc` (raw transaction bytes) | **Yes** | Bulk of the data; not needed for consensus once spent outputs are resolved through the indexes |
| `tx_loc_by_hash` | No | **Consensus-load-bearing.** Spending any UTXO resolves the outpoint via `outpoint → transaction_location() → OutputLocation → utxo_by_out_loc`. A UTXO from an old block can be spent at any later height, so this index can never be pruned by height window. |
| `hash_by_tx_loc` | No | Reverse of the above; kept for lookups |
| Consensus-critical state (UTXO set, value pools, note commitment trees, etc.) | No | Required for validation |

The key invariant: **deleting raw transaction bytes never removes information
needed to validate a future block.** Validation resolves spends through the
location indexes and the live UTXO set, neither of which is pruned.

## Switching between modes

Once a database has actually deleted data in pruned mode, the transition is
intentionally **one-way**:

| From → To | Allowed? |
| --- | --- |
| `archive → archive` | Yes |
| `archive → pruned` | Yes |
| `pruned → pruned` | Yes |
| `pruned → archive` | **Rejected** once data has been pruned |

### The pruning marker

The one-way transition is tracked by a single progress marker stored in the
`pruning_metadata` column family: `pruning_metadata[()] = lowest_retained_height`.

- If the marker is **absent**, the database is treated as not-yet-pruned and can
  still be opened as archive. This covers both ordinary archive databases and
  pruned-configured databases that have not yet reached the retention boundary.
- For compatibility with older databases, a _missing_ `pruning_metadata` column
  family is treated the same as a missing marker: not pruned.
- The marker is **written only when pruning actually deletes data** — not when
  pruned mode is merely configured. A node configured as `pruned` that has not yet
  synced past its retention window has no marker, and can still be switched back to
  archive.

The `pruning_metadata` column family is created lazily on any writable database,
archive included (where it stays empty). Adding it is a minor database format
version bump (`27.0.0 → 27.1.0`) within the same `state/v27/...` directory, so it
is an in-place upgrade with no re-sync. The upgrade framework reconciles the
version through a `NoMigration` entry; subsequent column-family additions should
bump the minor version again and add another `NoMigration` entry.

## Retention height and per-block pruning

The **lowest retained height** is the height boundary below which raw transaction
data has been deleted. Given a tip at height `T` and a retention window `R`, the
highest prunable height is `T - R`. After that height is pruned, the target
lowest retained height is `T - R + 1`: blocks at heights `[…, T - R]` may have
their raw transactions pruned, and blocks at heights `[T - R + 1, T]` are
retained.

### One height pruned per committed block

In steady state, pruning advances by exactly **one height per committed block**.
When a finalized block is committed at a new tip `T`, the retention boundary moves
up by one, making the single height `T - R` newly prunable. The prune is applied
inside the same per-block write, so the state size stays roughly constant as the
chain grows.

Pruning is wired directly into `ZebraDb::write_block`: when a pruning config is
present, `prune_height_range` computes the half-open height range `[from, until)`
to delete for this commit, and `prepare_prune_batch` adds those deletes to the
**same atomic `DiskWriteBatch`** that advances the tip. The prune and the tip
advance therefore commit together and can never be observed in an inconsistent
state. This rides the existing single-writer block-write path — there is no
separate background prune task, by design.

The genesis block (height 0) is never pruned.

### First online prune and marker catch-up

When pruning is first enabled on an existing archive database, online pruning does
not drain all historical raw transaction data below the retention boundary.
Instead, it starts at the current highest prunable height (`T - R`) and leaves
older archive history intact. This avoids turning the first pruned block commit
into a large historical cleanup.

`MAX_PRUNE_HEIGHTS_PER_COMMIT` (100) only matters after a pruning marker already
exists and falls behind the current retention boundary, for example if an
already-pruned node is reconfigured with a smaller `tx_retention`. In that case,
pruning resumes from the marker and catches up incrementally, bounded to 100
heights per commit. In steady state this cap is irrelevant because only one
height becomes prunable per block. The constant also reserves a tuning parameter
for a future batch-pruning implementation, if Zebra adds one. The range
arithmetic lives in the pure, unit-tested `prune_height_range_inner` function.

### The retention floor is load-bearing

`MIN_PRUNING_RETENTION` (10_000) is deliberately much greater than
`MAX_BLOCK_REORG_HEIGHT` (1000). At the current 75-second block target, this
retains about 8.7 days of raw transaction data, which is more than a week. The
correctness argument:

> We only ever delete at `tip - retention`, and a reorg only ever rewinds the
> last ≤1000 finalized blocks. Because `retention > MAX_BLOCK_REORG_HEIGHT`,
> pruning can never delete data that a rollback would need to read.

This floor is enforced at startup in `Config::validate_storage_mode`, before
RocksDB is opened. The floor is network-aware via `min_pruning_retention`: Mainnet
and Testnet use the full `MIN_PRUNING_RETENTION`, while Regtest uses a smaller
floor (`MAX_BLOCK_REORG_HEIGHT + 1`) suitable for short test chains. The floor
must not be lowered below `MAX_BLOCK_REORG_HEIGHT` without re-checking the
rollback path.

### Other invariants

- **Cumulative address balances are never height-pruned.**
  `balance_by_transparent_addr` is a running total, not per-height data; pruning it
  by height range would corrupt current balances.
- **Reads during batch construction see committed data.** `prepare_prune_batch`
  may read the database (for example, to enumerate transaction hashes) while
  building the batch; this is safe because the batch's own deletes are not yet
  visible. Do not assume otherwise.

## Pruning an existing database (offline tool)

Switching an already-synced archive node to pruned mode does not require a
re-sync. Zebra ships an offline tool to prune the finalized state in place, in two
forms:

- The `zebrad prune-state` subcommand, which defaults to `state.cache_dir` from
  the loaded `zebrad.toml`.
- A standalone `zebra-prune-state` binary, which defaults to
  `zebra_state::Config::default()`.

Both share the same implementation. Pass `--cache-dir` to be explicit and avoid
pruning the wrong (or an empty) state directory.

### Usage

```bash
# Preview the pruning plan without writing anything (the default).
zebrad prune-state \
    --network Mainnet \
    --cache-dir /path/to/state \
    --tx-retention 10000

# Apply the plan.
zebrad prune-state \
    --network Mainnet \
    --cache-dir /path/to/state \
    --tx-retention 10000 \
    --confirm
```

Flags:

- `--tx-retention` (required) — recent finalized blocks below the tip to keep.
- `--network` (required) — the network of the chain to load (`Mainnet`,
  `Testnet`, …).
- `--cache-dir` — path to the cached state directory.
- `--confirm` — actually apply the plan. Without it, the command only **previews**
  the plan and writes nothing.

Without `--confirm`, the tool calls `preview_prune_finalized_state` and prints the
plan; with `--confirm` it calls `prune_finalized_state` and applies it. Both print
a summary including the finalized tip, the retention window, the previous and new
lowest retained heights, and the pruned height range and count. After a successful
prune the database carries the pruning marker and is subject to the one-way
archive restriction described above.

## RPC behavior on a pruned node

On a pruned node, RPCs that need historical raw transaction data degrade
gracefully:

- `getrawtransaction` for a pruned transaction returns the existing not-found
  error. A per-txid "pruned vs never-existed" distinction is impossible once the
  bytes are gone — only a node-level hint ("this node is pruned; lowest retained
  height = N") is achievable, and is not currently surfaced.
- `getblock` for a pruned height depends on the verbosity:
  - `getblock <hash|height> 1` (the default; returns the header and the list of
    transaction IDs) does **not** read raw transaction bytes — it resolves the
    transaction IDs from the retained `hash_by_tx_loc` index — so it continues to
    work for pruned heights.
  - `getblock <hash|height> 0` (raw block) and `getblock <hash|height> 2` (full
    transaction objects) both require the raw transaction bytes, so they return
    the not-found error once those bytes have been pruned.

No consensus path reads raw transactions below the retained window: reorgs are
bounded to `MAX_BLOCK_REORG_HEIGHT` (1000) blocks, well within any valid retention
window (≥10_000), so the data a reorg needs is always retained.
