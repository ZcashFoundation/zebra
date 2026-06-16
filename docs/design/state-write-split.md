# Consensus / RPC-only State Write Split

- Status: **implemented as gated infrastructure on this branch (default OFF).**
  The single-DB path is unchanged and is the default. The split path is behind
  [`Config::separate_rpc_index_db`] and is exercised by unit tests; full-sync
  validation is sync-test-gated.
- Base: branch `ibd-engine`.
- Related: `docs/design/known-hash-ibd.md` §7.3 (the any-order write pipeline),
  `docs/design/p2p-snapshot-distribution.md` §1.1 (the RPC-only artifacts).

## 1. Motivation

The finalized-state write path is the measured IBD bottleneck (see the memory
note `project_ibd_state_write_bottleneck`): the disk-commit thread is CPU-bound
on transparent-address calculation and per-block `balance_by_transparent_addr`
GET+merge churn, while the disk itself is ~99% idle. None of that work is
needed to *validate* a block or to *reconstruct the chain* — it exists only to
answer address/balance/spent-tx RPC queries.

This document splits the finalized state into:

1. a **consensus DB** that holds everything block/transaction validation, spend
   resolution, the value pool, the note-commitment / history trees, and the IBD
   engine need — committed block-by-block on the existing any-order pipeline;
   and
2. an **RPC index DB** that holds only the address / balance / spent-tx
   indexes, written by a **second thread that trails the consensus DB**. The
   consensus thread never blocks on the RPC thread.

The whole split is gated; when it is off, every RPC-only column family is
written to the single main DB exactly as today.

## 2. Column-family classification

Every finalized-state column family in `STATE_COLUMN_FAMILIES_IN_CODE`
(`zebra-state/src/service/finalized_state.rs`), classified by whether it is
read on a consensus / chain-reconstruction / engine path, confirmed by grepping
every read site of each accessor in `zebra_db/transparent.rs`,
`zebra_db/block.rs`, `zebra_db/shielded.rs`, `zebra_db/chain.rs`, and
`service/read*`.

### 2.1 CONSENSUS-CRITICAL (stay in the main DB)

| Column family | Why it is consensus-critical |
|---|---|
| `hash_by_height` | block lookup, chain reconstruction, restart spot-check |
| `height_by_hash` | block lookup, reorg, non-finalized commit |
| `block_header_by_height` | header reads, commitment checks |
| `tx_by_loc` | transaction bodies, spend resolution, verification |
| `hash_by_tx_loc` | txid lookup |
| `tx_loc_by_hash` | txid → location, spend resolution |
| `utxo_by_out_loc` | **the unspent-output set**; read by spend resolution (`utxo`, `output_location`, `utxo_by_location`, `lookup_spent_utxos`) and the checkpoint commit's last-resort spend lookup |
| `sprout_nullifiers`, `sapling_nullifiers`, `orchard_nullifiers` | double-spend checks |
| `sprout_anchors`, `sapling_anchors`, `orchard_anchors` | anchor validity |
| `sprout_note_commitment_tree`, `sapling_note_commitment_tree`, `orchard_note_commitment_tree` | note-commitment trees (folded/derived during commit, read at tip) |
| `sapling_note_commitment_subtree`, `orchard_note_commitment_subtree` | subtree roots, consensus + light-client |
| `history_tree` | chain-history root, `hashBlockCommitments` |
| `tip_chain_value_pool` | value-pool consensus check |
| `block_info` (`BLOCK_INFO`) | per-block size/info, value-pool reconstruction |
| `known_hash_chunk` (`KNOWN_HASH_CHUNK`) | the P2P known-hash chunk store (engine hash source) |

### 2.2 RPC-ONLY (move to the RPC index DB when the split is on)

| Column family | Sole readers | Verdict |
|---|---|---|
| `utxo_loc_by_transparent_addr_loc` | `address_utxo_locations` → `address_utxos` / `partial_finalized_address_utxos` (RPC `AddressUtxos`) | RPC-only ✔ |
| `tx_loc_by_transparent_addr_loc` | `address_transaction_locations` → `address_tx_ids` / `partial_finalized_transparent_tx_ids` (RPC `TransactionIdsByAddresses`) | RPC-only ✔ |
| `balance_by_transparent_addr` (`BALANCE_BY_TRANSPARENT_ADDR`) | `address_balance_location` → `address_balance` / `address_location` (RPC `AddressBalance`, P2P snapshot range) | RPC-only ✔ **with one caveat — see §2.3** |
| `tx_loc_by_spent_out_loc` (`TX_LOC_BY_SPENT_OUT_LOC`) | `tx_location_by_spent_output_location` → `spending_tx_loc` (`#[cfg(feature = "indexer")]` RPC `spending_transaction_hash` only) | RPC-only ✔ |

`tx_loc_by_spent_out_loc` is only ever written/read under the `indexer` feature,
and only by the indexer RPC; it is never consulted by consensus. The two
address-index CFs and `tx_loc_by_spent_out_loc` are pure append/delete indexes
that no consensus or chain-reconstruction path reads.

### 2.3 The `balance_by_transparent_addr` caveat (load-bearing)

`balance_by_transparent_addr` is **read-modify-write on the consensus write
path during a normal sync**: `write_block` (`zebra_db/block.rs:635-643`) reads
each changed address's current balance (`address_balance_location`) to compute
the new balance (insert post-upgrade, merge during a format upgrade). The read
is *not* a consensus check — the value never gates block acceptance — but it
physically happens inside `write_block`.

Consequence for the split: the balance derivation must move **entirely** to the
trailing RPC thread, which reads the previous balance from, and writes the new
balance to, the **RPC index DB**. The consensus `write_block` must skip the
balance read and all four RPC-only passes when the split is on. The trailing
thread re-runs exactly the same `prepare_transparent_transaction_batch` logic
against its own DB, so the produced bytes are identical to the single-DB path
once it catches up. This is why the split reuses `FinalizedBlock` (which carries
the block plus its resolved `new_outputs`/spent UTXOs) rather than trying to
ship a pre-built cross-DB batch (a RocksDB `WriteBatch` is bound to one DB's CF
handles and cannot be split across DBs).

## 3. Two-DB layout

```
<cache>/state/v<major>/<network>/                 consensus DB (main)
<cache>/state/v<major>/<network>/rpc-index/       RPC index DB (split only)
```

- The RPC index DB is a second `ZebraDb` opened with **only the §2.2 CFs**
  (`RPC_INDEX_COLUMN_FAMILIES_IN_CODE`). The main DB, when the split is on, is
  opened with the §2.1 CFs only; when off, with the full list (unchanged).
- The RPC index DB carries its own durable tip marker (a small `rpc_index_tip`
  CF: height → block hash) so it knows, independently and crash-safely, how far
  it has indexed.
- `ZebraDb` gains an optional `rpc_index_db: Option<ZebraDb>` handle. The
  RPC-only read accessors consult `rpc_index_db` when it is `Some`, else fall
  back to the main DB (the default path).

## 4. The trailing RPC indexer thread

A third long-lived thread (after the worker T1 and disk writer T2), spawned by
the write pipeline only when `separate_rpc_index_db` is on:

- The disk writer (T2), after each durable consensus commit, sends the
  committed `FinalizedBlock` (it already owns it) down a **bounded** channel to
  the RPC indexer (T3). The channel is bounded so a stalled indexer eventually
  applies backpressure, but T2 only blocks if the indexer falls a full pipeline
  behind — in practice the indexer keeps up because it does strictly less work
  than the old combined commit and writes to an otherwise-idle disk.
- T3 builds the RPC-only batch for each block (the four §2.2 CFs) using the same
  `prepare_transparent_transaction_batch` code, reading prior balances from the
  RPC index DB, and writes it plus the advanced `rpc_index_tip` in one atomic
  RPC-index-DB batch.
- **The consensus thread never blocks on T3 for correctness** — only the
  bounded-channel backpressure couples them, and the channel is sized so that
  coupling is loose. If T3 panics, the channel closes; the consensus side logs
  and continues (RPC index simply stops advancing), because the RPC index is
  non-consensus data.

## 5. Crash safety and catch-up

The RPC index DB **trails** the consensus DB, so after a crash it may be behind
(never ahead — T3 only ever indexes blocks T2 has already made durable).

- **Invariant**: `rpc_index_tip` ≤ consensus finalized tip, always.
- **On open**: the indexer reads its own `rpc_index_tip` and the consensus tip.
  If `rpc_index_tip < consensus_tip`, it **catches up**: it walks
  `rpc_index_tip+1 ..= consensus_tip`, reconstructs each `FinalizedBlock` from
  the consensus DB (block + resolved spent UTXOs via the existing consensus
  accessors), indexes it, and advances `rpc_index_tip`. Only then does it begin
  consuming the live channel.
- Because the index for a height is a deterministic function of the block and
  the unspent set, re-indexing a height that was partially written is
  idempotent at block granularity (each height's batch is atomic, and a
  re-applied insert/delete reproduces the same bytes).
- Readers **tolerate trailing**: a read for an address whose most recent
  activity is above `rpc_index_tip` returns best-effort (the indexed prefix)
  rather than blocking. This is the same staleness RPC already accepts during
  IBD (`docs/design/p2p-snapshot-distribution.md` §3.3). The non-finalized
  overlay on top of the read is unchanged.

## 6. Config flag

```rust
/// Write RPC-only address/balance/spent-tx indexes to a separate "RPC index"
/// database, updated by a thread that trails the consensus database.
///
/// `false` by default: all column families live in one database and are
/// written together on the block-commit path, exactly as before.
pub separate_rpc_index_db: bool,
```

Default `false`. When `false`, none of the split code paths run: the main DB is
opened with the full CF list, `write_block` writes the RPC-only CFs inline, and
no trailing thread is spawned. Every existing test sees the unchanged path.

## 7. What is tested where

- **Unit (in `zebra-state`)**: with the split on, committing a block writes the
  consensus CFs to the main DB and the RPC-only CFs to the RPC index DB; the
  main DB contains none of the four RPC-only CFs' new keys; once the trailing
  thread catches up, the RPC read accessors return byte-identical results to the
  single-DB path; crash-catch-up replays a behind RPC index up to the consensus
  tip.
- **Sync-test-gated (the user runs)**: a full split-on sync, RPC parity against
  a single-DB sync at the same height, and the throughput delta.

## 8. Implementation status on this branch

See §8 of the report accompanying this change for the precise list of what is
wired end-to-end versus stubbed. The classification (§2), the config flag (§6),
the two-DB open/handle plumbing (§3), and the read-accessor seam (§3) are the
safe foundation; they are implemented so the default build and all existing
tests are unchanged.
