# Known-Hash Initial Block Download Engine

- Status: implemented on this branch; this document describes the system **as
  built** (the design was approved by the maintainer 2026-06-11; §15 logs how
  it evolved during implementation)
- Base: `main` @ `80709f096` (v5.1.0)
- Branch: `ibd-engine`
- Tracking issue: TBD

This document is the review artifact for the `ibd-engine` branch. Sections are
numbered stably — code comments cite them (e.g. `design doc §4.5`) — and each
phase in §9 maps to commits; reviewers should check each commit against the
spec section it implements rather than re-deriving the design from the diff.

**Contents**

- [The system in one page](#the-system-in-one-page)
- [1. Motivation](#1-motivation)
- [2. Trust and integrity model](#2-trust-and-integrity-model)
- [3. Architecture overview](#3-architecture-overview) — modules §3.1,
  constants §3.2, memory bounds §3.3
- [4. Component specifications](#4-component-specifications) — engine §4.1,
  batched fetch §4.2, verify-and-commit §4.3, commit pipeline §4.4, disk tier
  §4.5, commit failures §4.6, supervisor §4.7
- [5. zebra-network changes](#5-zebra-network-changes)
- [6. Asset strategy](#6-asset-strategy-the-103-mb-list--size-hints) — spec
  constant §6.1, size hints §6.2, distribution §6.3, residency §6.4
- [7. Consensus surgery](#7-consensus-surgery) — deletions §7.1, commit gate
  §7.2, state side §7.3, testnet §7.4
- [8. Observability](#8-observability)
- [9. Phases and commits](#9-phases-and-commits)
- [10. Config surface](#10-config-surface)
- [11. Validation gates](#11-validation-gates-before-release)
- [12. Throughput model and measurements](#12-throughput-model-and-measurements)
- [13. Risk register](#13-risk-register-abbreviated)
- [14. Explicitly deferred](#14-explicitly-deferred)
- [15. Design evolution log](#15-design-evolution-log)

## The system in one page

Zebra ships a SHA-256-pinned list of every mainnet block hash (plus a 1-byte
size hint per block). During initial sync, instead of discovering hashes from
peers round by round, one engine task downloads the whole pinned range by
hash, as fast as the network allows, and commits it in height order.

The life of one block, height `h`:

1. **Pinned**: `list[h]` is the block's hash, loaded from a verified chunk
   file (§6). The engine's window has one slot per uncommitted height.
2. **Fetched**: the engine issues a fetch future for `h` into a weighted
   batch layer (§4.2), which packs contiguous heights into one
   `BlocksByHash` request per ~800 KB (by size hint) or 16 hashes. The
   connection handler only accepts a block whose recomputed header hash
   matches the request — so an arrived block *is* the pinned block's header.
3. **Placed** (§4.5): on arrival the engine places the block — in memory
   while the RAM block buffer (`known_hash_lookahead_bytes`, 256 MiB default)
   has room; spooled raw to the disk overflow tier otherwise. The network
   never waits for the database: fetch-ahead continues onto disk up to 5× the
   RAM budget.
4. **Verified** (§4.3): when the commit pipeline has room, the engine pushes
   a verify-and-commit future. On the rayon pool, `convert` checks the
   coinbase height, the parent linkage against `list[h-1]`, and the merkle
   root against the transaction bodies (extending the header pin to the
   bodies, including the CVE-2012-2459 duplicate-txid check).
5. **Committed** (§4.4, §7.3): the state's any-order write pipeline commits
   the block — the worker updates the in-memory non-finalized chain (including
   the NU5+ `hashBlockCommitments` auth-data check) and responds; the disk
   writer writes the RocksDB batch with auto-compaction paused (§9 F2).
   Checkpoint-verified and semantically-verified blocks commit in any order.
   The engine's future resolves after the disk write; the slot is popped and
   the disk-tier copy (if any) is evicted.
6. **Protected** (§7.2): a stateless checkpoint gate in front of the
   semantic block verifier rejects any gossiped or RPC-submitted block at or
   below the mandatory checkpoint height (Canopy) — Zebra cannot fully
   validate that range, so only the engine commits it.

If a fetch fails it backs off and re-issues; a frontier-critical block stuck
in flight for 5 s gets a hedged single-hash refetch from another peer; a
block whose body fails verification is discarded and refetched. When every
height through `list.max_height()` is committed, the engine returns
`Completed` and the legacy syncer takes over from the real tip.

## 1. Motivation

Zebra's legacy initial sync discovers block hashes from peers
(`obtain_tips`/`extend_tips` FindBlocks rounds), downloads blocks one hash per
request, funnels every block through the `CheckpointVerifier`, and commits on
a single state write thread. With a bundled every-block hash list (3,358,432
mainnet hashes), the discovery step is unnecessary for ~99% of IBD — and once
hashes are known in advance, most of the remaining architecture is wrong by
construction:

1. **Discovery latency**: each FindBlocks round costs a network RTT plus
   hundreds of serial state reads, for hashes we already know (#10192).
2. **Serial verification floor**: all checkpoint CPU work (equihash, merkle
   root, transaction hashing) ran inside the consensus router's single
   `tower::Buffer` worker — one core, regardless of download parallelism.
3. **Restart churn**: unrecoverable errors trigger `cancel_all()` + a 67 s
   restart delay, frequently with thousands of downloads still in flight
   (#10192, #10438).
4. **Gap stalls**: a single missing block at the in-order commit frontier
   stalls the pipeline while later blocks pile up; nothing prioritizes
   refetching it.
5. **Startup waste**: the consensus router's background task walked the
   *entire* checkpoint list with one serial state request per entry on every
   restart — with an every-block list, up to ~3.36M requests through the
   write-path state buffer.

This branch replaces the checkpoint phase end-to-end with the **known-hash
IBD engine**: maximally parallel download and verification of blocks whose
hashes are pinned by a bundled, SHA-256-verified list, feeding the state in
strict height order. The `CheckpointVerifier` is deleted. The legacy syncer
(`obtain_tips`/`extend_tips`) remains for the post-IBD tail and steady-state
tip-following.

## 2. Trust and integrity model

The trust root is unchanged from the checkpoint model: hard-coded SHA-256
constants for the bundled hash-list chunks are reviewed at PR time, exactly
like the previous `main-checkpoints.txt` entries. Runtime verification proves
*file == pinned hash*; human review proves *pinned hash == canonical chain*.
The loader cross-checks `list[0] == network.genesis_hash()` and rejects
misaligned or truncated chunks.

What replaces the deleted checks:

| Check | Old location | New location | Justification |
|---|---|---|---|
| Block hash == expected hash | CheckpointVerifier hash-chain walk | `connection.rs` `Handler::process_message` (`pending_hashes.remove(&block.hash())`, covers multi-hash requests) + engine assigns hashes from `list[height]` | Header double-SHA256 recomputed on receipt; non-matching blocks ignored |
| Parent-hash linkage | CheckpointVerifier range walk | List pins it transitively; `convert` checks `block.header.previous_block_hash == list[h-1]`, converting list bugs from state-assert panics into fatal diagnostics; state asserts retained | Defense in depth |
| Coinbase height | `check_block` | `convert`: `coinbase_height() == Some(assigned_height)` | Pinned by merkle root |
| Difficulty threshold + check | `checkpoint.rs` | **Dropped** | `nBits` is in the pinned header |
| Equihash solution | `checkpoint.rs` | **Dropped** | `nSolution` is in the pinned header; re-verifying could only reject a block the list already commits to |
| Merkle root vs txids | `checkpoint.rs` via `merkle_root_validity` | `convert`, **same function** (`zebra_consensus::block::check::merkle_root_validity`) | **Must be retained**: the block hash pins only the header; this check extends the pin to transaction bodies. Tx hashes are computed anyway for the state request, so it is nearly free |
| Duplicate-txid (CVE-2012-2459) | inside `merkle_root_validity` | retained automatically (same call) | Reuse, never re-implement |
| Tx branch-ID consistency | inside `merkle_root_validity` | retained automatically | |
| NU5+ auth-data commitment | state `commit_finalized_direct` (never in CheckpointVerifier) | state write pipeline Thread 1: `commit_checkpoint_block` runs `block_commitment_is_valid_for_chain_history` (`hashBlockCommitments` vs history tree + auth data root) before the in-memory commit | v5 txids do not commit to witnesses; substituted auth data is caught at commit time → the implicated copy is discarded and the height refetched (§4.6) |
| ≤4 queued blocks per height | `MAX_QUEUED_BLOCKS_PER_HEIGHT` | engine window holds exactly one slot per height, byte-capped | Strictly stronger |
| Mandatory checkpoint floor (Canopy) | router `init_checkpoint_list` | `CheckpointGateLayer` (§7.2): a stateless layer in front of the semantic verifier rejects commits at or below `mandatory_checkpoint_height()` | A constant of the network, not a function of config |
| State-vs-list consistency on restart | router background full-list walk | O(1) spot-check: `BestChainBlockHash(tip)` vs the checkpoint at or below the tip | Fixes the 3.36M-request startup walk |

Consensus-acceptance change: **none**. Every block the engine commits is
pinned byte-for-byte (header by hash, body by merkle root, v5 auth data by the
`hashBlockCommitments` check on the commit path). Checks that were never run
on the checkpoint path (signatures, UTXO validity, subsidy, time) are still
not run below the final known height.

## 3. Architecture overview

The engine lives in `zebrad/src/components/ibd/` and talks only to
zebra-network (batched `Request::BlocksByHash`) and zebra-state
(`Request::CommitCheckpointVerifiedBlock`). No zebra-consensus *service* is on
the path; two pure zebra-consensus helpers are reused:
`primitives::spawn_fifo` (rayon FIFO + oneshot) and
`block::check::merkle_root_validity`.

```
                              zebrad ibd engine — ONE task, no internal channels
                    ┌──────────────────────────────────────────────────────────────┐
                    │ ENGINE (engine.rs)                                           │
 zebra-network      │  window: VecDeque<Slot>, one slot per uncommitted height,    │
┌────────────┐      │          grows/shrinks with the active span (≤16,384 blocks) │
│  PeerSet   │      │  blocks: FuturesUnordered<BlockFut> — staged per-block       │
│ (Buffer'd) │      │          futures, issued in height order while under caps    │
└─────▲──────┘      │  policies: refill, disk fetch-ahead, gap hedging, stalls     │
      │             └───────┬──────────────────────────────────────────────────────┘
      │ one BlocksByHash    │ stage 1: fetch        stage 2: verify + commit
      │ per flushed batch   ▼                            ▼
┌─────┴────────────────────────────────┐      ┌──────────────────────────────────┐
│ BATCHED FETCH (fetch.rs)             │      │ VERIFY-AND-COMMIT (convert.rs)   │
│ tower-batch-control over a          ─┼──────► spawn_fifo(convert) on rayon:    │
│ FetchBatcher inner service;          │      │ tx hashing + merkle + linkage,   │
│ weight = list size hint (§6);        │      │ then CommitCheckpointVerified-   │
│ flush at 16 items / ≥800 KB weight   │      │ Block; future resolves after the │
│ / 10 ms latency. Gap hedges are      │      │ RocksDB write                    │
│ single-hash requests (route_inv)     │      └───────────────┬──────────────────┘
└──────────────────────────────────────┘                      │
                            cache.rs ◄── disk overflow tier   ▼
                            (raw blocks, §4.5)   ┌───────────────────────────────┐
                                                 │ StateService (Buffer'd)       │
                                                 │  → checkpoint write pipeline: │
                                                 │  T1 commit_checkpoint_block   │
                                                 │     (in-memory + respond)     │
                                                 │  T2 commit_finalized_direct   │
                                                 │     (RocksDB, compaction      │
                                                 │      paused, §9 F2)           │
                                                 └───────────────────────────────┘
```

Ownership rules: one owner per piece of state, no shared mutexes anywhere in
the new code. The engine task owns the window, the byte accounting, and the
disk cache (all cache I/O is synchronous, page-cache-bound, and called
directly from the loop). The supervisor (`ibd.rs`) owns the restart loop and
reports the outcome to startup wiring.

### 3.1 Module layout

```
zebrad/src/components/ibd.rs                 IbdEngine supervisor, IbdOutcome, restart loop, handoff
zebrad/src/components/ibd/
├── engine.rs               the engine task: window, staged per-block futures, refill,
│                           disk fetch-ahead, gap hedging, stall warnings
├── fetch.rs                FetchRequest (RequestWeight impl) + FetchBatcher inner service
│                           for tower-batch-control; NotFound/transport classification;
│                           fetch_single (the hedge path)
├── convert.rs              convert() + VerifyAndCommit tower service over the state
├── cache.rs                disk overflow tier: download each block at most once
└── tests.rs + tests/       cache.rs, convert_vectors.rs

zebra-chain/src/parameters/known_hashes.rs   KnownHashListSpec + windowed chunk loader (§6)
zebra-chain/src/parameters/known_hashes/     *.bin chunk assets (in git; excluded from crates.io)

zebra-state/src/service/write.rs                       any-order write pipeline: WriteMessage + spawn (§7.3)
zebra-state/src/service/write/worker.rs                the single write-worker loop + 4 message handlers (§7.3)
zebra-state/src/service/write/disk_writer.rs           the persistent disk-writer thread (sole commit_finalized_direct caller)
zebra-state/src/service/write/finalized_write_phase.rs RAII compaction-pause / WAL-skip guard (§9 F2)

zebra-utils/src/bin/known-hashes/            list assembly tool: RPC sweep, anchors, chunk emission (§6.2, §7.4)
```

(Module layout convention: `foo.rs` + `foo/` directory, never `foo/mod.rs`.)

### 3.2 Named constants

Recorded here so reviewers can check the code against the spec; the
config-overridable values are in §10.

| Constant | Value | Rationale |
|---|---|---|
| `IBD_BATCH_MAX_BLOCKS` | = `GETDATA_MAX_BLOCK_COUNT` (16) | wire serving limit; overflow is **silently dropped** by serving nodes — aliased to the inbound constant so the two sides can't drift |
| `IBD_BATCH_MAX_WEIGHT` | = `GETDATA_SENT_BYTES_LIMIT` − 200,000 (800 KB) | tower-batch-control `max_items_weight_in_batch` — a flush-after-crossing threshold: all-but-the-last item sum to < 800 KB; derived from the serving limit, with a 200 KB margin for hint quantization (§4.2) |
| `IBD_BATCH_FLUSH_LATENCY` | 10 ms | tower-batch-control `max_latency`: fills batches without meaningful per-block latency |
| `ITEM_WEIGHT_FLOOR` | 50,000 (fetch.rs) | = ceil(800 KB / 16): the per-item weight floor that makes the 16th item cross the weight threshold, so the block-count and weight caps cooperate |
| `SIZE_HINT_UNIT` | `MAX_BLOCK_BYTES.div_ceil(255)` = 7,844 B (zebra-chain) | size-hint quantum: hint `w` (1..=255) means serialized size ≤ `w × SIZE_HINT_UNIT`; single source shared by emitter and engine |
| `DEFAULT_SIZE_HINT` | 255 (fetch.rs) | conservative hint for heights whose chunk has no embedded hints yet (§6.2) — single-block batches, safe |
| `HASHES_PER_CHUNK` | 150,000 (zebra-chain) | hashes per chunk file; single source shared by emitter and the bundled specs |
| lookahead budget | 256 MiB (config `known_hash_lookahead_bytes`) | **the RAM block-buffer budget** — the block-count lookahead is not configured; it emerges from the budget and actual block sizes (§4.1) |
| `DISK_FETCH_AHEAD_FACTOR` | 4 | the disk tier's fetch-ahead allowance as a multiple of the RAM budget: fetching stops when RAM-held + disk-held bytes reach budget × 5 (§4.5) |
| `IBD_WINDOW_MAX_BLOCKS` | 16,384 | operational cap on the window's block count: the refill walk is O(window) per loop event, and in small-block eras the byte budgets alone would allow hundreds of thousands of blocks of fetch-ahead, starving the loop |
| `IBD_SPAN_MAX` | 2,000,000 | **defensive ceiling only** (≈80 MB of slot bookkeeping if accounting breaks); never binds in practice — the real bounds are the byte budgets and the window cap |
| `IBD_MAX_CONCURRENT_BATCHES` | 96 | the batch layer is built once at this ceiling (its `max_concurrent_batches` is fixed for the worker's lifetime); the **live** limit is enforced at issuance: `min(3 × ready_peers, 96)`, at least 1 |
| `IBD_COMMIT_PIPELINE_BLOCKS` | 1,024 | max unresolved verify-and-commit futures (the only real state backpressure signal) |
| `IBD_COMMIT_PIPELINE_BYTES` | 64 MiB | byte bound on the same |
| `IBD_FRONTIER_CRITICAL_SPAN` | 64 | heights from the commit frontier treated as gap-critical |
| `IBD_GAP_HEDGE_AFTER` | 5 s (config `known_hash_gap_hedge_secs`) | age of a frontier-critical in-flight slot before a hedged single-hash refetch |
| `IBD_HEIGHT_RETRY_BACKOFF` | 500 ms × 2^attempts, cap 30 s | per-height retry pacing |
| `IBD_CACHE_EVICT_INTERVAL` | 1,024 | frontier heights between batched disk-tier eviction rounds (bounds linger time, not deletion count) |
| `COMMIT_FAILURE_ATTEMPT_LIMIT` | 3 | deterministic-failure detector (§4.6) |
| `IBD_RESTART_DELAY` | 15 s | supervisor delay between engine restarts |
| `IBD_MAX_RESTARTS_WITHOUT_PROGRESS` | 5 | zero-progress restarts before degrading (above the mandatory floor only) |
| `IBD_STALL_WARN_INTERVAL` | 60 s | frontier-stall warning cadence |

### 3.3 Memory-bound audit (DoS review anchor)

Every allocation on the engine path and its bound. The design rule is:
**block bytes live in exactly one accounted place at a time**, and every
container is bounded by bytes and count (blocks range ~1.6 KB – 2 MB; the
2 MB protocol cap is enforced at deserialization in zebra-network, so no
single response item can exceed it).

| Structure | Bound | Enforced by |
|---|---|---|
| In-flight batch responses (network + batch-control queue) | live concurrent-batch limit (`min(3 × ready_peers, 96)`) × (800 KB + one crossing block ≤ ~2 MB) ≈ 269 MB worst case, realistically ≪ (spam-era batches are single-block) | issuance gate (`fetch_slots_available`) + tower-batch-control queue limits |
| RAM block buffer (`Slot::Fetched` blocks + promoted blocks in the commit pipeline) | `known_hash_lookahead_bytes` (256 MiB default), counted exactly from arrival sizes; the frontier block may overshoot by ≤ one block (it always stays in memory, §4.5); a failed cache write keeps its block in memory (bounded by the in-flight cap) | arrival-time placement: over-budget arrivals go to the disk tier |
| Verify-and-commit pipeline (rayon queue + state queue + write channel, jointly) | `IBD_COMMIT_PIPELINE_BLOCKS` (1,024) / `IBD_COMMIT_PIPELINE_BYTES` (64 MiB) unresolved futures — futures resolve only after the disk write | engine stage-boundary caps (§4.4) |
| State `finalized_state_queued_blocks` + write channel | transitively ≤ the commit-pipeline caps: **only the engine submits during IBD** (the §7.2 gate excludes gossip/RPC below Canopy, and callers don't submit in the engine's range above it); the worker→disk-writer channel itself is bounded (capacity `checkpoint_sync_pipeline_capacity`, min 500) | engine caps + gate + bounded channel |
| Window slot bookkeeping | ≤ `IBD_WINDOW_MAX_BLOCKS` (16,384) slots in practice (~40 B each); `IBD_SPAN_MAX` (2,000,000 ≈ 80 MB) is the defensive ceiling if accounting breaks | window cap in `refill`; span ceiling |
| Disk overflow tier | disk, not RAM: RAM-held + disk-held bytes ≤ budget × (1 + `DISK_FETCH_AHEAD_FACTOR`) = 5 × budget (~1.25 GB at the default; transient double-storage of blocks the chain state is about to store anyway); in-memory index is one `(height, hash, u32)` entry per cached block | `storage_allows` issuance gate; continuous eviction on commit |
| Known-hash list | ≤ 2 resident chunks (~5–10 MB, hints embedded); all chunks verified at open then dropped; the whole list is dropped once the tip passes `max_height` | windowed loader (§6.4) |

Unsolicited or duplicate peer data never enters these structures: the
connection `Handler` drops blocks that don't match a requested hash, and
hedge duplicates are dropped at the single per-height window slot. The
state's queues are unbounded *types*, but during IBD their only writer is the
engine. Final assurance is empirical: gate 4 (§11) pins the process RSS
ceiling, and the §8 gauges (`ibd.window.bytes`, `ibd.lookahead.blocks`,
`ibd.cache.bytes`) make each bound observable.

## 4. Component specifications

### 4.1 Engine task (`engine.rs`)

One task owns everything: a window over the uncommitted height range and one
`FuturesUnordered` of per-block staged futures. There are no internal data
channels and no second stateful task — one byte-accounting point. (The only
external control input is the peer set's status `watch`, which sizes the
fetch concurrency.)

```rust
enum Slot {
    Unrequested { attempts: u8, not_founds: u8, backoff_until: Option<Instant> },
    InFlight    { attempts: u8, not_founds: u8, since: Instant,
                  hedged: Option<AbortHandle>, abort: AbortHandle },
    Fetched     { block: Arc<Block>, bytes: u32, committing: bool,
                  source: Option<PeerSocketAddr> },   // in the RAM buffer; retained
                                                      // until its commit resolves
                                                      // (§4.6 reset recovery — same
                                                      // Arc the state holds)
    Cached      { bytes: u32, promoted: bool },       // in the disk tier (§4.5)
    Committed,                                        // awaiting the frontier pop
}

struct Engine {
    window: VecDeque<Slot>,   // one slot per height in [base, base + len);
                              // grows and shrinks with the active span
    base: Height,             // lowest uncommitted height; pop_front on advance
    blocks: FuturesUnordered<BlockFut>,   // staged per-block futures
    budget_used: u64,         // exact RAM-held block bytes
    // services: batched_fetch (§4.2), verify_and_commit (§4.3), cache (§4.5)
}
```

Per-block work is **staged futures in one collection** — one task, one
`FuturesUnordered`, but the engine observes the fetch→commit boundary (this
is load-bearing: it is how the commit-pipeline caps are enforced and how
`Slot::Fetched` retention works):

- **Stage 1 — fetch**: resolves when the block arrives (or with a classified
  failure). The engine decides the block's placement **at arrival**: the
  frontier block and under-budget arrivals become `Slot::Fetched` in memory;
  over-budget arrivals are spooled raw to the disk tier (§4.5). Stage-1
  futures carry an `AbortHandle` so hedge losers can be cancelled.
- **Stage 2 — verify + commit**: pushed by the engine for `Fetched` slots
  (and for `Cached` slots being promoted) **only while unresolved stage-2
  futures are under the §4.4 pipeline caps**; otherwise the block waits in
  its slot. The future is one `VerifyAndCommit::oneshot` call (§4.3) and
  resolves after the RocksDB write.

Main loop, per iteration: `refill` → `hedge_frontier` → gauges → stall
tracking, then `select!` over the next staged-future completion and the
earliest pending timer (backoff expiry, hedge deadline, stall warning). After
handling one completion, the loop **drains every already-ready completion**
before the next pass, so the O(window) refill and stall scans run once per
ready batch instead of once per future. If no future is in flight and no
timer is pending while the range is uncommitted, that is a broken invariant
and a fatal `EngineError`.

`refill` walks the window lowest-missing-first:

1. **Promote / commit**: a `Cached` slot is promoted (read from disk through
   the normal §4.3 verify path), and a `Fetched` slot gets its stage-2 push —
   both while the §4.4 caps hold. The frontier height bypasses the caps (its
   commit unblocks everything else, and after a §4.6 recovery the pipeline
   may be full of parked descendants).
2. **Issue fetches** while three gates hold: `storage_allows` (RAM-held +
   disk-held bytes under the total allowance, §4.5), `fetch_slots_available`
   (in-flight fetches under `min(3 × ready_peers, 96) × 16`), and the window
   under `IBD_WINDOW_MAX_BLOCKS`. The block-count lookahead is not configured
   anywhere — it emerges from the byte budgets and actual block sizes
   (small-block eras hit the window cap; the sandblasted era fetches a few
   hundred ahead). Memory-vs-disk placement happens at arrival, not at
   issuance. Batch formation is not the engine's job: contiguous
   height-ordered issuance into the weighted batch layer (§4.2) yields
   contiguous, size-fitted batches by construction.
3. **Gap hedging** (a separate pass over the frontier-critical span): any
   height in `[base, base + IBD_FRONTIER_CRITICAL_SPAN)` still `InFlight`
   after `IBD_GAP_HEDGE_AFTER` gets a second, **single-hash** fetch
   (`fetch_single`, bypassing the batcher). A single-hash `BlocksByHash`
   gets the peer set's inventory-aware routing (`route_inv`) for free, and
   the one-request-per-connection rule lands it on a different peer than the
   still-pending primary by construction. First completion wins; the slot's
   `AbortHandle` cancels the loser, and a hedge that fails or is promoted
   restarts the slot's hedge clock (no re-hedge spin).

Retry policy is owned by the engine loop (futures return classified
outcomes; they never retry internally beyond §4.2's one explicit-notfound
round). Failed heights back off `500 ms × 2^attempts` (cap 30 s) and
re-issue; explicit `notfound`s are counted per slot and in the
`ibd.peer.notfound.count` metric. There is no per-peer exclusion or scoring
machinery: routing is the peer set's job, and a recorded exclusion the
router never consults is dead weight (§15.1; misbehavior reporting through
the address book is a `TODO(known-hash-ibd D6)`).

**Stall ladder**: 1. normal assignment → 2. hedge after 5 s (frontier-critical
span) → 3. back off and reassign on each failure (exponential, capped 30 s)
→ 4. after 60 s with no fetch-frontier progress, `warn!` with height /
attempts / notfounds / ready-peer count + the `ibd.stall.seconds` gauge,
repeating every interval. (A planned step — `MorePeers` demand to the address
book after repeated attempts — is a recorded TODO.) The engine owns its range
and never permanently abandons it.

Degradation policy (split at the mandatory checkpoint): below
`mandatory_checkpoint_height` (Canopy − 1; 1,046,399 on mainnet) the engine
loops forever with alarms — semantic sync below Canopy is not a sound
fallback. Above it, after `IBD_MAX_RESTARTS_WITHOUT_PROGRESS` (5) consecutive
supervisor restarts with zero frontier progress, the engine returns
`Degraded` and the legacy syncer takes over (correct, just slower), behind an
explicit `warn!` (see §7.2's open follow-up on the below-checkpoint handoff).

### 4.2 Batched fetch service (`fetch.rs`)

Batching is a **tower-batch-control** layer (already weight-aware on main:
the `RequestWeight` trait, `max_items_weight_in_batch`, and a `max_latency`
flush) over a `FetchBatcher` inner service — the same `Service<BatchControl<R>>`
pattern as the batch crypto verifiers; each `Item` call returns its own
response future, so per-block response distribution is native:

- `FetchRequest { height, hash, size_hint }` implements `RequestWeight` as
  `max(hint × SIZE_HINT_UNIT, ITEM_WEIGHT_FLOOR)` — the floor makes the 16th
  item cross the weight threshold, so the count cap and the weight cap
  cooperate (tower-batch-control has no separate item-count parameter; the
  batcher also hard-chunks any flush at 16 hashes). Batch parameters:
  `max_items_weight_in_batch = IBD_BATCH_MAX_WEIGHT` (800 KB),
  `max_concurrent_batches = IBD_MAX_CONCURRENT_BATCHES` (96, the fixed
  ceiling — the live limit is the engine's issuance gate),
  `max_latency = IBD_BATCH_FLUSH_LATENCY` (10 ms).
- **Overflow analysis (verified)**: the worker admits the crossing item
  *before* flushing, so a flushed batch's hint weight is < 800 KB **plus at
  most one block** (worst ≈ 2.8 MB). This is still served in full: zebrad's
  serving loop includes at least one block before applying the 1 MB limit
  and checks *before* each subsequent block — since every prefix of an
  honest batch except the final block sums to < 800 KB < 1 MB, no block is
  ever trimmed; zcashd's send-buffer throttling defers rather than drops.
  The 200 KB margin under the serving limit absorbs hint quantization.
- `FetchBatcher` accumulates `Item(FetchRequest)`s (stashing each item's
  responder); on `Flush` it issues one
  `network.ready().await?.call(BlocksByHash(batch))` through the existing
  Buffer'd peer set (no tower Hedge/Retry stack; hedging is the engine's
  smarter job; the 20 s `REQUEST_TIMEOUT` bounds each round), then matches
  `Available` blocks to items by hash and fulfills each responder with the
  block, its (already-matched) hash, and the source address.
- Since the engine issues requests in height order, flushed batches are
  contiguous height runs by construction (modulo interleaved retries — fine:
  every height is deep history, servable by any peer).

**Response semantics (verified on main)**: a timeout fails the *whole*
request (accumulated blocks are discarded by the connection handler) —
classified as transport loss for every item in the batch, with no peer
penalty beyond EWMA load. Partial responses exist only when the peer sends
explicit `notfound` (zebrad does, zcashd doesn't): `Missing` entries are
classified `NotFound`, `Available` entries are used. One immediate
re-request round inside `FetchBatcher` is allowed for the missing remainder
of an explicit-notfound partial; everything else resolves the item future
with a classified error so the engine's retry policy (backoff, hedging)
decides.

**Inventory-registry contract (#10679/#5709)**: the engine never writes to
the registry. Marking stays in the client layer
(`MissingInventoryCollector`), which only forwards explicit
`Missing`/`NotFoundResponse` — transport errors never poison routing. Error
classification in `fetch.rs` mirrors the same taxonomy: explicit NotFound ⇒
"peer lacks block"; timeout/drop/overload ⇒ transport, meaning nothing about
the peer's inventory.

### 4.3 Verify-and-commit tower service (`convert.rs`)

Verification is a tower middleware **on top of the buffered state service**,
using zebra's existing rayon bridge `zebra_consensus::primitives::spawn_fifo`
(sends a closure to the rayon FIFO pool and returns the result on a oneshot).
One `call()` per block = verify on rayon, then commit to the state; the
returned future resolves only after the RocksDB write. This removes the
single-Buffer-worker verification floor: conversions run in parallel across
the global rayon pool (sized by `sync.parallel_cpu_threads`), shared with the
batch-crypto verifiers — which are idle during IBD, so the engine gets the
whole pool, and they share fairly via FIFO near the handoff.

The engine threads the pinned hashes at issuance
(`IbdBlock { height, expected: list[h], prev_expected: list[h-1], block, source }`),
so the verify stage needs no list access of its own. The payload is either a
deserialized `Arc<Block>` (memory tier — its header hash already matched the
pin in the connection handler) or raw bytes from the disk tier, which are
deserialized **inside the rayon closure** and re-checked against the pinned
hash before `convert` runs (cached bytes are untrusted input, §4.5).

```rust
// pure, runs on rayon
fn convert(network, height, expected, prev_expected, block: Arc<Block>)
    -> Result<CheckpointVerifiedBlock, ConvertError>
{
    if block.coinbase_height() != Some(height)           { return Err(WrongHeight); }
    if block.header.previous_block_hash != prev_expected { return Err(BrokenLink); }
    // tx hashing + new_outputs — the expensive part, parallel across blocks
    let cvb = CheckpointVerifiedBlock::with_hash(block.clone(), expected);
    // The header hash pins only the header; extend the pin to tx bodies.
    // Reuses zebra_consensus::block::check::merkle_root_validity VERBATIM —
    // carries the CVE-2012-2459 duplicate-txid check and branch-ID consistency.
    check::merkle_root_validity(network, &block, &cvb.transaction_hashes)?;
    Ok(cvb)
}
```

Failure semantics (errors carry `source` for attribution):
`BadMerkleRoot`/`DuplicateTransaction` ⇒ the delivering peer sent a corrupt
body for a real header — the copy never met the §4.5 invariant condition, so
the slot resets for a refetch (with the base backoff, so a peer serving the
same bad body can't drive a tight CPU spin); misbehavior reporting through
the address book is a recorded TODO. `CorruptCachedBytes` ⇒ the disk-tier
entry is discarded and the height refetched (§4.5 exception (a)).
`WrongHeight`/`BrokenLink` ⇒ list inconsistency — **fatal diagnostic** naming
height and hashes; never a retry loop. Commit-stage errors follow §4.6.

### 4.4 Commit pipeline and state protection (within the engine task)

There is no separate feeder task: commits are stage-2 futures in the
engine's single collection (§4.1), and the caps are enforced at the stage
boundary — the engine pushes a stage-2 future only while unresolved stage-2
futures are under `IBD_COMMIT_PIPELINE_BLOCKS` (1,024) /
`IBD_COMMIT_PIPELINE_BYTES` (64 MiB). Fetched blocks above the caps wait in
their `Slot::Fetched` entries; the frontier height bypasses the caps (§4.1).

Why this protects the state: `CommitCheckpointVerifiedBlock` enqueues
non-blocking into the state's parent-hash-keyed queue; the only true
backpressure signal is the commit-completion future (which resolves after
the disk write). Capping unresolved commits bounds everything downstream.
Ordering: the state's finalized queue drains in **parent-chain order from
the last-sent hash regardless of arrival order** (out-of-order arrivals park
in the queue) — so fetches and rayon conversions completing out of order are
safe by construction. The engine issuing futures in height order plus
`spawn_fifo`'s FIFO ordering keeps completions nearly in order in practice,
so the state queue stays ~empty — a memory optimization, not a correctness
requirement.

Two deliberate properties:

- **Slot retention until resolution**: the engine keeps the fetched block's
  `Arc` in its `Slot::Fetched` until the commit future resolves. This is
  free (the state queue holds the same `Arc`) and is what lets §4.6 resubmit
  reset-dropped descendants from memory instead of refetching them.
- **No depth-dependent timeouts**: verify-and-commit futures have no analog
  of the 480 s `BLOCK_VERIFY_TIMEOUT` trap; liveness is monitored by
  frontier progress, not per-future timers. Corrupt bodies are detected at
  the verify stage — up to 1,024 blocks ahead of the disk frontier — so the
  refetch budget is the whole pipeline's drain time, usually costing zero
  frontier stall.

Dedup: one slot per height is the single dedup point; hedge losers are
cancelled via the slot's `AbortHandle`, and a lost hedge race that still
delivers is dropped at the slot.

Genesis is simply `list[0]` fetched by hash — no `request_genesis` special
case in the engine. (The state write pipeline commits genesis directly to
disk, because the non-finalized chain initializes its tree state from a
finalized tip; regtest commits its genesis directly from startup wiring.)

### 4.5 Disk overflow tier (`cache.rs`) — download each block at most once

**Invariant**: a block copy is never downloaded again once **(1)** its
header hash matches the pinned known-hash list entry for its height **and
(2)** its body has been confirmed to belong to that header. Condition (1) is
established at receipt (the connection `Handler` hash check) and re-pinned
by `convert`. Condition (2) is the merkle-root check in `convert` — run
exactly once per block: inside the commit call for memory-tier blocks, at
promotion for disk-tier blocks. It fully confirms the body for v1–v4
transactions (txids commit to the full serialized transaction); for v5
transactions the merkle root confirms the effecting data, and the
authorizing data is only confirmable at commit time (`hashBlockCommitments`
needs the history tree). Consequences:

- A copy that fails `convert` never met the condition — refetching it is
  *inside* the invariant, not an exception.
- A v5 copy that passes merkle but fails its auth-data commitment at commit
  never met condition (2) — the implicated copy is discarded and the block
  refetched, also inside the invariant.
- A copy that met the condition is sacred: it lands on disk rather than
  being dropped, and is promoted from disk rather than refetched. Remaining
  exceptions are only (a) disk corruption of a cached entry (caught by
  re-verification, refetched) and (b) process-crash or shutdown loss of
  in-memory copies (bounded by the RAM budget plus in-flight fetches).

**The cache is the window's second tier, not an exception path.** The
engine keeps downloading **past the RAM budget**, saving those blocks to
disk instead of holding them in memory while the state works through the
commit pipeline. Placement is decided **at arrival** against the live RAM
counter:

- The frontier block always stays in memory (its commit unblocks everything
  else — a bounded ≤ one-block budget overshoot).
- An under-budget arrival becomes `Slot::Fetched` (RAM, counted exactly).
- An over-budget arrival is written to the cache **raw, with no verification
  beyond the connection `Handler`'s header-hash match**, and its slot
  becomes `Cached`. Verifying before the write would be pure double work
  (cached bytes are untrusted on load, so promotion must re-run `convert`
  regardless), and the saved tx-hashing pass matters most in the spam era —
  exactly where the disk tier is most active. A failed cache write keeps the
  block in memory instead (never waste a download).
- Fetch issuance stops only when RAM-held + disk-held bytes reach
  `budget × (1 + DISK_FETCH_AHEAD_FACTOR)` — the network stays busy whenever
  it is faster than the state. There is no separate disk-tier config: cached
  blocks are blocks the chain state is about to store anyway, so the
  transient double-storage is space the volume must have regardless,
  continuously evicted as commits pass.

As commits free RAM budget, `refill` **promotes** the lowest cached heights:
a synchronous ≤ 2 MB page-cache-bound file read in the loop (the cache is
single-owner, no mutex — see the module docs), then the raw bytes flow
through the same §4.3 verify path as a network block (deserialization inside
the rayon closure). Promoted blocks count against the RAM budget until their
commit resolves. **Each block is verified exactly once in the common case**;
the bounded exceptions are re-*verification* of bytes already held
(commit-reset resubmission §4.6, restart restore), never re-download —
gate 8 counts downloads.

Mechanism — one file per block under the state cache directory
(`<cache_dir>/ibd-block-cache/<height>-<hash>.bin`): a one-line sidecar
(`zebra-ibd-cache-v1 <source-addr>`) followed by the canonical
`zcash_serialize` bytes, written in a single pass. No per-block fsync — torn
writes are caught by re-verification at promotion. The sidecar stores the
**source peer address** so a corrupt body discovered at promotion still gets
attribution. (The real address is stored: this is a local file, not a log or
metric.)

- **Restart**: on engine (re)start, scan the cache dir, prune entries
  outside `(state_tip, list.max_height()]`, and mark surviving heights
  `Cached`. Cached bytes are **untrusted input**: on load they go through
  the full `convert` path (hash vs `list[height]`, merkle, linkage) exactly
  like a network response.
- **Eviction**: entries at or below the commit frontier are deleted in
  lazily-batched rounds (every `IBD_CACHE_EVICT_INTERVAL` = 1,024 frontier
  heights — the interval bounds how long committed entries linger, not how
  many are deleted). The directory is removed entirely at `Completed`
  handoff. A copy whose body fails verification is deleted individually.

This decouples network throughput from state-commit throughput: in
write-bound eras the network races ahead onto disk and only goes idle when
the allowance fills; transient slow patches (peer churn, gap stalls) no
longer drain the pipeline. Honestly stated: the disk tier smooths variance
and keeps the network saturated — steady-state throughput is still the
slowest stage's rate, and the tier adds one sequential write+read per
overflow block on the state volume.

A planned hardening (a `known_hash_cache_write_ahead` config, off by
default, added together with its implementation) would also spool
memory-tier blocks to the cache on arrival, closing exception (b) at the
cost of one extra sequential write of the chain (~100 GB) across IBD.

### 4.6 Commit-failure handling

On a commit error, the state write task drops the failed block's queued
descendants (its reset path). The engine recovers without re-downloading:
descendants are **resubmitted from the `Arc`s retained in their
`Slot::Fetched` entries** (§4.4) — a re-*verification* through `convert`,
never a re-download. Only the failed height itself is refetched: the
implicated copy (held or cached) is discarded, its budget released, and the
slot reset with the base backoff (routing may return the refetch to the same
peer; reporting the source through the address book is a recorded TODO —
this matters for NU5+ auth-data substitution, which is only detectable at
commit). After `COMMIT_FAILURE_ATTEMPT_LIMIT` (3) failures of a
**byte-identical** block, declare deterministic failure (list-vs-chain
inconsistency or local DB corruption), log a fatal diagnostic, and stop the
engine with an explicit error — never silent fallback, never infinite
redownload. A closed state service (`StateUnready` / write task exit) is
fatal-shutdown: the engine returns `Err` and startup task supervision tears
the process down.

### 4.7 Supervisor, restart, handoff (`ibd.rs`)

```rust
pub enum IbdOutcome {
    Completed { final_height: Height },  // committed through list.max_height()
    Declined(DeclineReason),             // NoList | AlreadyPast | DisabledByConfig
    Degraded,                            // repeated zero-progress restarts above
                                         // the mandatory floor (§4.1)
}
```

`run()` is a plain re-enterable function: every (re)start re-derives
`next_commit` from `best_tip_height() + 1`, re-opens the list (reading and
SHA-256-verifying the ~103 MB asset set on a `spawn_blocking` thread —
restarts are rare enough that re-verifying beats holding a second copy),
restores the disk tier (§4.5), and rebuilds the rest of the window empty;
bounded loss ≤ the RAM-tier bytes. The tip is checked against the spec
constant *before* opening the list, so a synced node never re-hashes the
assets. A missing or tampered asset set is a hard, actionable error — with
the checkpoint verifier deleted, the engine is the only path that can commit
blocks at or below the mandatory floor, so there is no silent fallback
(§6.3). The supervisor restarts after `IBD_RESTART_DELAY` (15 s) on
fatal-but-retryable errors; the consecutive-failure counter resets whenever
a restart makes frontier progress.

Wiring (start.rs): `ChainSync::new` is still constructed at startup (its
`SyncStatus`/`RecentSyncLengths` handles feed mempool, gossip, progress, and
the health endpoints), but `sync()` is driven only after the engine returns.
With no `RecentSyncLengths` pushes during the engine phase,
`is_close_to_tip()` is `false` — the correct "mempool inactive, not ready"
semantic, with no shim. The engine task is awaited inside the same supervised
task as the syncer, so ctrl-c and sibling-task failures abort it cleanly.

After `Completed`/`Declined`/`Degraded`, the legacy syncer starts from a
block locator at the real tip — no special seam. There is no
finalized→non-finalized "flip" to coordinate: the state write worker commits
checkpoint-verified and semantically-verified blocks in any order (§7.3), so
the first semantically-verified child of the last committed block is simply
committed, with no phase transition to time against the disk tip.

## 5. zebra-network changes

Re-implemented fresh on main (the syncer-performance originals predate 8
months of `set.rs` drift), one concern per commit:

1. **Live peer height + height-aware routing**: `LoadTrackedClient` gains a
   monotonic live height raised by delivered blocks
   (`remote_height() = max(handshake, live)`); multi-hash `BlocksByHash`
   routes via tall-peer-filtered P2C. Mostly inert during the deep-history
   engine phase; directly improves the legacy tail.
2. **Stalled-tip eviction**: when the chain tip stalls for 10 minutes *and
   at least one ready peer reports a height above ours*, evict one random
   peer and signal `MorePeers` (rate-limited to one per window). The
   peer-ahead condition keeps eviction off synced, quiet-network, and
   regtest nodes, where dropping a healthy peer is pure harm.
3. **Per-peer sync diagnostics**: `blocks_received` counters + rate-limited
   peer set status logs (ready/unready counts, height distribution, per-peer
   delivered blocks and EWMA load). The operator's visibility into IBD.
4. **`PeerSetStatus` watch**: the ready-peer count, published on a watch
   channel from `zebra_network::init` and consumed by the engine to size its
   live fetch concurrency (§4.2). Deliberately minimal — fields are added
   when a consumer lands.

Explicitly **not** changed: inventory-registry semantics (the #10679 fix is
consumed, never modified); the connection `Handler` state machine. (A
separable follow-up may return accumulated blocks on timeout instead of
discarding them — safe for #10679 because absence ≠ `Missing` — but the
engine does not depend on it.)

## 6. Asset strategy (the ~103 MB list + size hints)

### 6.1 The spec constant

Per network, a compile-time constant pins everything a consumer needs
**without loading any chunk**:

```rust
pub struct KnownHashListSpec {
    /// The highest height covered by the list (3_358_431 on mainnet today).
    pub max_height: block::Height,
    /// The number of block hashes in each chunk file (except the last):
    /// HASHES_PER_CHUNK = 150,000, shared with the emitter.
    pub chunk_blocks: u32,
    /// SHA-256 of each chunk file, as lowercase hex, in chunk order.
    pub chunk_hashes: &'static [&'static str],
    /// The chunk file name prefix (e.g. "main-known-hashes").
    pub file_prefix: &'static str,
}
```

`max_height` being a reviewed constant (not derived from file sizes) means
the §7.2 gate floor and state-init parameters need no chunk I/O at all, and
a truncated or extended asset set is detected as a hard mismatch at load
(every chunk's length and hash must match the spec).

### 6.2 Per-block size hints

Each chunk file carries the raw 32-byte hashes for its heights followed by
**1 byte per block**: the block's serialized size as a fraction of the
maximum block size, **rounded up** —
`hint = serialized_size.div_ceil(SIZE_HINT_UNIT)` with
`SIZE_HINT_UNIT = MAX_BLOCK_BYTES.div_ceil(255)` (= 7,844 B), so
`hint × SIZE_HINT_UNIT` is always an upper bound. The loader accepts both
chunk formats (hash-only and hashes-then-hints) and validates hints into
`1..=255`; heights without embedded hints get `DEFAULT_SIZE_HINT` (255 —
single-block batches, safe). Mainnet chunks 00–14 (heights 0..=2,249,999)
embed hints; chunks 15–22 are hash-only until their sizes are swept from a
synced node and re-emitted (a recorded `TODO(ibd-engine E2)`).

Uses:

- **`RequestWeight` for the batched fetch layer** (§4.2): batches pack to
  the 800 KB flush threshold with a priori per-block sizes — no adaptive
  size tracker, no estimate drift; rounding up keeps every pre-crossing
  prefix under the serving limit.
- A wrong hint is liveness-only (mis-packed batch → silent truncation →
  handled as transport loss), never a correctness issue.

Generation: the `zebra-utils` `known-hashes` tool sweeps a synced local node
via `getblockhash`/`getblock` (explorers were evaluated and rejected for
bulk collection — page-scraping 3.4M blocks takes weeks at polite rates; a
local RPC sweep takes under an hour). `SIZE_HINT_UNIT`, `HASHES_PER_CHUNK`,
and the chunk file-name format are defined once in
`zebra_chain::parameters::known_hashes` and consumed by both the emitter and
the loader, so the two sides cannot disagree.

### 6.3 Distribution and integrity

- The `KnownHashListSpec` constants stay **in source** (the trust root never
  leaves review). Chunks live in `zebra-chain/src/parameters/known_hashes/`
  in git, but are **excluded from the published crate** (`Cargo.toml`
  `exclude`; crates.io would reject a ~103 MB package).
- Runtime resolution order (`KnownHashList::search_dirs`):
  (1) the `[sync] known_hash_list_dir` config override; (2)
  executable-adjacent (`./known-hashes/`, `../share/zebrad/known-hashes/` —
  release tarballs and docker images ship chunks here; published chunks are
  append-only so docker layers dedupe across releases); (3) the platform
  data dir (e.g. `~/.local/share/zebrad/known-hashes`); (4) the development
  tree (`CARGO_MANIFEST_DIR`), so `cargo run` works without installation.
- A tampered chunk is a hard error naming the chunk. A missing asset set is
  also a hard, actionable error (the error text names the config and the
  search locations): with the checkpoint verifier deleted, the engine is
  the only path that can commit the pinned range, so silent fallback would
  strand the node (§4.7).
- **Planned**: verified self-download of missing chunks from pinned HTTPS
  URLs (each chunk verified against its baked-in SHA-256 before write and
  re-verified on load), added together with its `known_hash_list_download`
  config field. Until then, missing chunks are the hard error above.

### 6.4 Memory residency: windowed chunk loading

Disk reads are cheap — re-reading 100 MB even several times is fine;
*retaining* it is the waste. Residency model (`KnownHashList`):

- **At open**: read every chunk once, verify all SHA-256s against the spec
  constant — then **drop the bytes** (fail fast on tampering without holding
  103 MB). Opening runs on a blocking thread (§4.7).
- **During the engine phase**: hold at most **2 resident chunks** (the raw
  ~5 MB chunk bytes, indexed in place — hashes by offset, hints from the
  tail section), each re-verified against its pinned SHA-256 on load, evicted
  LRU. The engine releases heights below the frontier
  (`release_below(base - 1)`; the frontier's parent stays resident for the
  linkage pin), and the window cap (`IBD_WINDOW_MAX_BLOCKS` = 16,384 <
  150,000 chunk span) keeps the active range inside two chunks. A chunk
  fault is a ~5 MB read + SHA-256 (~20 ms, once per 150,000 heights),
  deliberately synchronous in the refill step — cheaper than threading file
  I/O through the per-block futures.
- **After the chain tip passes `max_height`**: the engine (and its list) are
  dropped entirely. The §7.2 gate needs only the `max_height` constant; the
  restart spot-check uses the spaced checkpoint list.

The spaced `checkpoint_list()` BTreeMap stays for the legacy path, the
restart spot-check, and tooling.

## 7. Consensus surgery

### 7.1 Deletions (done)

| Area | What died |
|---|---|
| `zebra-consensus/src/checkpoint.rs` + `checkpoint/types.rs` + tests | the entire CheckpointVerifier (~2,100 LOC) |
| `router.rs` | the `BlockVerifierRouter` service and `RouterError` (the router was first reduced to a gate, then folded into the semantic verifier, then extracted as the stateless `checkpoints.rs` layer — §7.2); `router.rs` itself was dissolved into `lib.rs` (init + background task) and `checkpoints.rs` (heights + gate); the background full-list state walk |
| `zebrad` sync.rs / downloads.rs | checkpoint lookahead branches and checkpoint concurrency constants (`MAX_CHECKPOINT_*` now live in `zebra-chain`/`zebra-node-services` for the remaining consumers) |

`CheckpointList` itself stays in zebra-chain (testnet construction
validation, `zebra-checkpoints` tooling, the restart spot-check, progress
ETA). `CheckpointVerifiedBlock` and `CommitCheckpointVerifiedBlock` stay —
they are the engine's commit path.

### 7.2 The commit gate

Zebra cannot fully validate blocks at or below the mandatory checkpoint
height (Canopy activation): some consensus rules are not implemented for
those network upgrades, so the only sound way to commit them is
checkpoint-equivalent verification — the known-hash engine's pinned list.

As built, the gate is a **stateless tower layer**
(`zebra_consensus::checkpoints::CheckpointGateLayer`), wrapped around the
semantic block verifier by `zebra_consensus::init`: any commit or proposal
request at or below `network.mandatory_checkpoint_height()` is rejected with
`VerifyBlockError::BelowMandatoryCheckpoint { height, floor }` before it
reaches the verifier or the state service. The verifier itself processes any
block above the mandatory height, with no engine knowledge; callers are
responsible for routing checkpoint-verifiable blocks through the engine, and
for not sending blocks in the engine's range to the verifier unless
checkpoint sync is disabled in the config. (Regtest has no list and a zero
mandatory floor, so the gate is inert there.)

Gossip's downloader already treats verifier errors as non-fatal drops; the
legacy syncer surfaces `BelowMandatoryCheckpoint` as an actionable error
(re-downloading cannot help; enable known-hash sync) instead of a silent
retry loop; RPC `submit_block` maps the error to a rejection — including for
blocks the state already holds, since the stateless gate fires before the
verifier's already-in-chain check.

The old router's background list-walk is replaced by an O(1) spot-check
(`BestChainBlockHash` at the spaced checkpoint at or below the tip),
preserving wrong-chain-on-restart detection without the walk.

A semantically-valid block submitted inside the engine's range mid-IBD (its
parent the engine's current finalized tip, above Canopy) is now **safe by
construction**: the state write worker commits checkpoint-verified and
semantically-verified blocks in any order (§7.3), so such a block simply forks
the in-flight chain and is reorged away by the hash-pinned prune if it loses —
no early "flip", no special caller responsibility. Inbound gossip and the
legacy syncer don't submit in this range in practice anyway (gossip fetches
near the network tip; the syncer is idle during the engine phase).

**Open / planned follow-ups (maintainer decisions):**

- *Degraded handoff below the mandatory checkpoint.* When the engine
  degrades below the mandatory height, the legacy syncer it hands off to
  still cannot commit that range (the gate holds). The choice — keep
  restarting the engine forever with alarms, vs. exit fatally — is an
  operational-behavior decision.
- *Flag-off fresh mainnet sync* below Canopy is unsupported without the
  engine (no checkpoint verifier); whether to hard-error at startup is open.

### 7.3 State side: the any-order write pipeline

`zebra_state::init` keeps its `max_checkpoint_height` parameter, but startup
wiring now passes the **max finalizable height** — the checkpoint max raised
to the list max while the engine is enabled — so the finalized write path
and backup-restore cover the whole pinned range. (Renaming the parameter to
match its raised meaning is a recorded follow-up.) The near-final-checkpoint
UTXO window keeps its `checkpoint_verify_concurrency_limit`-based semantics.

The state's write path is **one persistent worker loop** over **one input
channel**, feeding **one persistent disk-writer thread** — the sole caller of
`commit_finalized_direct` in the whole system. Checkpoint-verified and
semantically-verified blocks commit **in any order**, interleaved: there is no
finalized→non-finalized "flip". The two threads are:

- **The worker** (`write/worker.rs`, Thread 1) reads `WriteMessage`s
  (`Checkpoint`, `Semantic`, `Invalidate`, `Reconsider`) from one
  `tokio::mpsc` channel and commits each block to the in-memory non-finalized
  state. The single-threaded `StateService` serializes both block streams into
  commit order before they reach the channel, so a FIFO read preserves
  parent-before-child ordering with no cross-channel races and no locks.
  Checkpoint blocks go through `commit_checkpoint_block` (tree updates,
  nullifiers, UTXOs, spent-output resolution, and the NU5+
  `hashBlockCommitments` auth-data check, §2), which is **fork-tolerant**
  (it locates the parent chain by tip hash and sources chain context from it)
  and **validate-before-mutate** (every fallible check runs before any chain
  mutation, so an error leaves the non-finalized state untouched). The worker
  hands each durable-bound block to the disk writer over a bounded crossbeam
  channel (capacity = `checkpoint_sync_pipeline_capacity`, min 500), and
  **responds to the state service as soon as the block is in memory**.
- **The disk writer** (`write/disk_writer.rs`, Thread 2) drains its channel
  through `commit_finalized_direct` (keeping the tip-linkage assertions,
  `debug_stop_at_height`, and elasticsearch indexing the serial loop had) and
  writes the RocksDB batch. It owns the `FinalizedWritePhase` RAII guard as a
  reversible `Option`: created on the first **bulk** write (a checkpoint-stream
  block), dropped on an `EndBulk` message, a non-bulk write, or channel close.
  The guard pauses auto-compaction for the bulk-write phase (and skips the WAL
  iff `state.disable_wal_during_ibd`), with a level-0 file-count hysteresis
  (re-enable compaction above 64 L0 files, pause again at 32). It restores
  everything on every exit path; the next database open re-enables compaction
  after a `kill -9`.

**Disk frontier and prune.** The worker tracks `next_disk_height` and
`disk_frontier_hash` (the last block handed to the disk writer), and an
`inflight_disk` queue of handed-off checkpoint blocks still in the
non-finalized state. The disk writer publishes its on-disk tip height on a
`Release`/`Acquire` atomic; the worker prunes **hash-pinned** — a block is
finalized out of memory only when its own height is observed durable, by its
exact hash via `finalize_root(Some(hash))`. This closes the hole where a
transient adversarial fork briefly out-working the pipeline chain could make a
work-based prune pop the fork's root and orphan the pipeline chain. It also
makes the restored-backup property structural: only `inflight_disk` entries are
ever pruned, and those hold only worker-enqueued blocks, so restored blocks
can never be pruned.

**Any-order gate (checkpoint commits).** For a checkpoint block whose parent
is a chain tip (or the finalized tip), the worker commits it directly — the
pure-bulk hot path. Otherwise:

- if an identical block already entered memory via a semantic commit (a
  handoff-window race), the worker **adopts the twin** and responds `Ok` (the
  ack contract is already satisfied);
- if the block is above the disk frontier, the worker **flushes its
  semantically-committed ancestors** to the disk writer first (those ancestors
  got strictly more validation than checkpoint blocks), failing safe with no
  mutation on a fork below the frontier;
- otherwise (stale, ahead, or a sibling), the worker responds
  `CommitBlockError::OutOfOrder { height, next_height }`. The known-hash IBD
  engine answers any error above its frontier by resubmitting its retained
  copy (a re-verification, never a re-download), so an explicit `OutOfOrder`
  beats silently dropping the block.

**Bulk lifecycle.** The recently-finalized `PrunedChain` cache is enabled
lazily on the first checkpoint commit; the first semantic commit drops the disk
writer's bulk guard (via `EndBulk`) and the cache. A later checkpoint block
re-enables both — re-enabling the cache empty is always correct (lookups fall
back to database reads).

**Genesis** commits directly to the disk writer with a blocking ack (the
non-finalized chain initializes its trees from a finalized tip), folding away
the old `FinalizedState::commit_finalized` special case. **Reorg-overflow**
roots (best chain past the 1000-block rollback window) likewise go to the disk
writer with a blocking ack.

**Commit-response contract** (unchanged): a checkpoint commit response means
**the block is committed in memory; its disk write is in flight**, at most a
pipeline depth behind. A genesis commit response means the block is durable.
Blocks in flight are not readable through the state until their disk writes
land — the worker does **not** publish the non-finalized state to the watch
channel during checkpoint commits (keeping the chain `Arc` uniquely owned for
in-place mutation and the backup task idle). Semantic commits and successful
admin ops always publish.

**Admin ops during IBD.** Invalidate and reconsider now work during checkpoint
sync for blocks **above** the disk frontier; a block at or below the frontier
(its disk write enqueued, in flight, or complete) is rejected with a typed
error. Invalidating the only non-finalized chain empties the state and
publishes the empty snapshot, falling the chain tip back to the finalized tip.

**Error policy** (cited in the worker module docs):

| Path | Handling |
|---|---|
| Checkpoint not extending the canonical tip (stale, ahead, sibling) | respond `OutOfOrder`; no reset |
| Same-hash twin already in the canonical chain | respond `Ok` (idempotent adopt) |
| Ancestor-flush linkage mismatch (competing fork below the frontier) | respond `OutOfOrder`; no mutation |
| Genesis disk write error (ack path) | error metrics + respond Err + reset(db tip); continue |
| Checkpoint in-memory commit error (auth-data substitution, spend/value build) | respond Err + reset(parent); continue; non-finalized state untouched |
| `Chain::push` failure after all checks passed | expect-panic (internal invariant) |
| Post-ack checkpoint disk error (`ack: None`) | disk writer panics (documented fatal), propagates via scope join |
| Overflow-root disk error (`ack: Some`) | worker expect-panic (trees already validated) |
| Semantic contextual error | respond Err + parent-error map + rejected-hash channel |
| Invalidate/Reconsider below the disk frontier | typed Err, no state change |

The checkpoint in-memory commit error replaces a previously remote-triggerable
panic: a substituted-signatures NU5 block passes every engine check and fails
only the auth-data check on the commit path, so that path must be recoverable.
Because the commit is validate-before-mutate, the reset rewinds the service to
the parent and the honest copy recommits cleanly; the engine's refetch /
three-strike machinery (§4.6) becomes reachable.

Consensus-review note: like the old `commit_finalized_direct`-only path, the
checkpoint path skips the Sapling/Orchard anchor checks (the pinned hashes
guarantee the contents); the auth-data commitment check was *added* relative
to the old path (§2). The exact same checks run on the exact same blocks — only
threading, ordering tolerance, and failure handling changed.

### 7.4 Testnet (pending — E2)

The spaced testnet list cannot drive known-hash sync (intermediate hashes
are unknown). Strategy: **assemble the testnet every-block list anchored
against the existing spaced checkpoints** (a pure asset addition to the
network-generic loader; ~3.1M blocks ≈ ~100 MB).

1. The `zebra-utils` `known-hashes` tool assembles every testnet block hash
   and block size 0..tip from a synced local node (`getblockhash`/`getblock`
   sweep).
2. **Anchor verification**: every height present in the reviewed,
   already-trusted spaced `test-checkpoints.txt` (10,144 entries, ≤400
   apart) must match exactly — one mismatch fails the run. This roots the
   assembled data in the same trust anchor the node ships today.
3. Cross-checks: explorer spot-checks at sampled heights (a random sample
   plus all network-upgrade activation heights). Disagreement anywhere fails
   the run.
4. The tool emits the chunked `.bin` format (hashes + embedded hints) and
   prints the `KnownHashListSpec` constant block ready to paste; the
   loader's genesis/alignment checks apply as on mainnet.

Defense in depth for intermediate (non-anchor) heights: the engine requests
each block by its pinned hash and checks `prev_hash == list[h-1]` linkage at
convert time, and every ≤400-block run terminates in a
spaced-checkpoint-anchored hash — a wrong intermediate hash can only cause
NotFound stalls (liveness), never acceptance of an off-chain block.

Until the testnet list lands, testnet IBD from scratch is **broken by
design** on this branch (the engine declines with `NoList` and the gate
holds the mandatory floor): documented honestly rather than worked around.

## 8. Observability

- Metrics (per CLAUDE.md prefixes): `ibd.fetched.block.count`,
  `ibd.converted.block.count`, `ibd.committed.height`, `ibd.window.bytes`,
  `ibd.inflight.batches`, `ibd.gap.hedge.count`, `ibd.stall.seconds`,
  `ibd.peer.notfound.count`, `ibd.lookahead.blocks` (the emergent
  auto-scaled lookahead — directly shows the per-era self-tuning),
  `ibd.cache.bytes`, `ibd.cache.written.count`, `ibd.cache.promoted.count`,
  `ibd.cache.restored.count`, and `ibd.duplicate.download.count` (the §4.5
  invariant made observable — increments only on the documented exceptions,
  with a `reason` label ∈ {`corrupt-cache`, `unconfirmed-copy`}), plus a
  compatibility increment of `sync.downloaded.block.count` so existing
  dashboards keep working.
- The `state.checkpoint.*` and `zcash.chain.verified.block.*` families are
  emitted by the state itself and survive unchanged (this also keeps the
  `create_cached_database` acceptance regex alive).
- Progress: `progress.rs` keeps working unmodified (reads `latest_chain_tip`
  + `sync_status`); `getblockchaininfo` `verificationprogress` is
  tip-derived and unaffected. The engine logs frontier progress and stall
  warnings on the `IBD_STALL_WARN_INTERVAL` cadence.

## 9. Phases and commits

Each phase landed as the listed commits (squashed/amended during review;
see `git log` for the final shapes). Nothing changes default behavior before
Phase D6/E1.

### Phase A — Spec

| # | Commit | Status |
|---|---|---|
| A1 | `docs(design): add known-hash IBD engine design` | done (this document; updated as the design evolved — §15) |

(The A2 benchmark-harness cherry-pick was dropped from the branch; the
harness is rebuilt at F4, deferred — §14.)

### Phase B — Known-hash list infrastructure (zebra-chain)

| # | Commit | Status |
|---|---|---|
| B1 | `feat(chain): add KnownHashList with verified windowed chunk loader` | done — spec constant §6.1, layered search + residency §6.3–6.4, 23 mainnet chunks, `Cargo.toml` exclude |
| B2 | `feat(chain): embed per-block size hints in known-hash chunks` | done — chunks 00–14 re-emitted with hints (§6.2); 15–22 pending the E2 sweep |

### Phase C — zebra-network peer health and routing

| # | Commit | Status |
|---|---|---|
| C1 | `feat(network): live peer height tracking and height-aware block routing` | done (§5.1) |
| C2 | `feat(network): rate-limited peer eviction on stalled chain tip` | done (§5.2; peer-ahead gate added in review) |
| C3 | `feat(network): per-peer block delivery counters and peer set diagnostics` | done (§5.3–5.4) |

### Phase D — The IBD engine (zebrad)

| # | Commit | Status |
|---|---|---|
| D1 | `feat(zebrad): add IBD engine skeleton behind sync.known_hash_sync` | done |
| D2 | `feat(ibd): engine task with ring window and weighted batched fetch` | done (§4.1–4.2) |
| D3 | `feat(ibd): verify-and-commit tower service over the state with rayon merkle checks` | done (§4.3) |
| D5 | `feat(ibd): disk overflow tier so each block is downloaded at most once` | done (§4.5) |
| D4 | `feat(ibd): commit pipeline with gap-priority hedging and reset recovery` | done (§4.4, §4.6; includes the arrival-placement rework, §15.1) |
| D6 | `feat(zebrad): run IBD engine before legacy syncer when enabled` | done (§4.7) |

### Phase E — Default-on and consensus removal

| # | Commit | Status |
|---|---|---|
| E1 | `feat(zebrad)!: enable known-hash sync by default on mainnet` | done (validation gates §11 pending maintainer runs) |
| E2 | `feat(chain): add testnet every-block known-hash list` | **pending** — needs a synced testnet node (§7.4); the assembly tool (`zebra-utils known-hashes`) is done |
| E3 | `refactor(consensus)!: remove checkpoint verifier and gate commits by height` | done (§7.1–7.3); the gate was subsequently folded into the semantic verifier and the router removed |

### Phase F — State write performance

| # | Commit | Status |
|---|---|---|
| F1 | `perf(state): tune RocksDB for write-heavy initial sync` | done (write buffers, background jobs; no format change) |
| F2 | `perf(state): pause auto-compaction during known-hash IBD with exit-safe guards` | done (§7.3: RAII guard, L0 hysteresis, opt-in `disable_wal_during_ibd`) |
| F3 | `perf(state): pipeline checkpoint commits across lookup/write stages` | done (§7.3 two-thread pipeline; review findings fixed in follow-up commits) |
| F4 | rebuild the per-era benchmark harness with cached seed states | **deferred** (§14) |

A review-and-fix wave followed the initial stack (auth-data gap, frontier
deadlock, frozen batch concurrency, eviction gating, floor semantics, router
removal), then a cleanup wave (dead code, shared constants, doc accuracy,
hot-path efficiency) — see `git log` and §15.

## 10. Config surface

Additive fields on the existing `[sync]` section (serde `default` keeps old
configs parsing under `deny_unknown_fields`):

```rust
/// Enable known-hash initial sync (default on; Mainnet is currently the only
/// network with a bundled list).
pub known_hash_sync: bool,                 // true
/// Initial-sync lookahead, as a RAM byte budget for fetched-but-uncommitted
/// blocks. The block-count lookahead is not configurable: it emerges from
/// this budget and actual block sizes (§4.1).
pub known_hash_lookahead_bytes: usize,     // 268_435_456 (256 MiB)
/// Seconds a frontier-critical block may be in flight before a hedged refetch.
pub known_hash_gap_hedge_secs: u64,        // 5
/// Override directory for the known-hash chunk files.
pub known_hash_list_dir: Option<PathBuf>,  // None → layered search (§6.3)
```

And one `[state]` field:

```rust
/// Skip the RocksDB write-ahead log during the initial-sync bulk-write phase
/// (opt-in: trades crash-resync time for write throughput on slow disks).
pub disable_wal_during_ibd: bool,          // false
```

Planned fields, added together with their implementations:
`known_hash_list_download` (verified self-download of missing list chunks,
§6.3) and `known_hash_cache_write_ahead` (spool every fetched block to the
cache, §4.5).

The legacy fields are unchanged: `checkpoint_verify_concurrency_limit` still
sizes the state's UTXO lookahead window (§7.3) and the state's queued-block
memory limits; `download_concurrency_limit`, `full_verify_concurrency_limit`,
and `parallel_cpu_threads` still drive the legacy tail and the rayon pool.

## 11. Validation gates (before release)

1. **Tip-hash identity**: `getblockhash` equals a main-built node at heights
   {1, 10k, 100k, 419,200, 1,046,400, 1,687,104, 2,000,000, 3,000,000,
   3,358,431}; identical committed tip after steady state.
2. **Zero stalls**: one full genesis→list-max mainnet run with zero fatal
   engine restarts; 24 h soak with induced peer churn (drop 50% randomly)
   always recovers.
3. **Throughput**: ≥ 2× aggregate via full-run wall clock (interim signal);
   per-era blocks/s ≥ main in every era via the rebuilt F4 harness once it
   lands.
4. **Memory**: every §3.3 bound holds; `ibd.window.bytes` never exceeds the
   budget; RSS ≤ main + 700 MB (256 MiB lookahead + worst-case in-flight
   batches + resident chunks + slack).
5. **Crash safety**: kill -9 at three random heights (one during paused
   compaction) → resumes from tip+1, DB healthy, cached blocks restored
   without refetch.
6. **Testnet/regtest unaffected**: regtest suite green; flag-off mainnet
   with an existing post-Canopy state syncs; (full testnet parity waits on
   E2 — §7.4).
7. **Config compat**: a v5.1.0 zebrad.toml parses and runs.
8. **No duplicate downloads**: `ibd.duplicate.download.count` stays 0 across
   the full run, the soak, graceful restarts, and induced commit-reset tests
   — exceptions only with `reason` ∈ {`corrupt-cache`, `unconfirmed-copy`}
   and each occurrence explained (`unconfirmed-copy` = a copy that failed
   convert or its commit-time auth-data check, i.e. never met the §4.5
   invariant condition — invariant-consistent, but counted for
   observability).
9. *(E2 additionally)*: testnet list landed + gates 1–2 re-run on testnet;
   mid-IBD gossip/RPC injection is handled safely by the any-order write
   worker (§7.3) — it forks the in-flight chain and is reorged away if it
   loses; a backup-restore cycle behaves correctly with the new final height
   (~3.36M vs ~1M today).

## 12. Throughput model and measurements

Design-time estimates (150 ms RTT, 4 MB/s per peer, 1 Gbps downlink, 8
cores): small-block eras bind on the **state write thread**; the medium era
binds on the **network** at ~25 peers; the large/sandblasted era binds on
**node downlink**. Convert never binds once parallel.

Measured so far on this branch (WSL2, NVMe, real mainnet — see the
validation-gate runs for the authoritative numbers):

- **Pre-Overwinter (~0–150k)**: ~375–400 blocks/s steady state, **network
  /peer-count bound** — ~700–800 ms per 16-block batch round-trip × ~20–25
  reachable peers. CPU ~1.4 cores total; the engine extracts close to the
  per-peer round-trip limit. Reachable-peer count (~20–25) is a network
  environment ceiling, not a Zebra config limit (config experiments with
  target sizes up to 160 did not move it).
- **Integrated genesis→107k** (engine + state writes on one machine):
  ~242 blocks/s, **CPU-bound on the state pipeline's Thread 2**
  (`commit_finalized_direct`: transparent-address computation + per-address
  balance reads against the compaction-paused memtables/L0), with the disk
  ~1% utilized. Per-stage timing: Thread 2 ≥ Thread 1 consistently (e.g.
  2.2 ms vs 0.8 ms per block at 50k). Measured-and-rejected experiments:
  more flush parallelism (no gain — a single hot column family flushes
  serially) and disabling the compaction pause (worse: 164 blk/s at 2.5×
  the CPU). Identified-but-deferred levers: an in-memory address-balance
  cache during IBD, per-CF selective compaction, computing the auth-data
  root on the rayon pool (§14).

No performance claim here is final until the rebuilt harness (F4)
reproduces it; the interim signal is full-run wall clock from the gate 1–2
runs.

## 13. Risk register (abbreviated)

| Risk | Sev | Mitigation |
|---|---|---|
| Gossip/RPC block at engine-tip+1 disrupts the mid-IBD commit pipeline | Critical | §7.2 gate below Canopy; above it, safe by construction — the any-order write worker (§7.3) forks the in-flight chain and reorgs it away if it loses; injection test (gate 9) |
| Merkle/dup-txid check dropped in convert | Critical | reuse `merkle_root_validity` verbatim; body-swap vectors (D3) |
| v5 auth-data substitution → commit-reset amplification | High | commit-time `hashBlockCommitments` check (§2) + 3-failure byte-identical deterministic detector (§4.6); implicated copy discarded; refetch backoff |
| Scheduler recreates #10679 poisoning | High | engine never writes the registry; only explicit NotFound is classified as inventory knowledge |
| Window OOM in 2 MB-block era | High | bytes-first budgets; arrival-time placement; frontier-only overshoot bounded to one block |
| One-shot fallback regression (ibd-minimal defect) | High | re-enterable `run()`; the engine loops forever below the mandatory floor |
| Installed-binary asset resolution breaks | High | layered loader (§6.3); hard actionable error, never silent |
| Testnet IBD broken by deletion | High | documented (§7.4); E2 lands the list; gates re-run on testnet |
| Compaction/WAL toggle leaves DB degraded on crash | Med | RAII guard + re-enable-on-open + kill test (F2) |
| Oversized getdata batches → silent 20 s stalls | Med | 16-item cap + 800 KB threshold derived from the serving limit; serve-after-one analysis (§4.2); crafted crossing-batch test |
| Wrong/stale size hints mis-pack batches | Low | liveness-only (truncation → transport loss); hints pinned by the chunk hashes; `DEFAULT_SIZE_HINT` for unhinted chunks |
| Eclipse/total stall looks like success | Med | frontier watchdog + 60 s warns + not-ready health status |
| Cache poisoning / corrupt disk-tier entries accepted | High | cache is untrusted input: full `convert` re-verification on load (hash vs list, merkle, linkage); corrupt entries discarded + refetched |
| Disk tier peaks large when state lags network | Low | bounded at 5× the RAM budget (§4.5); continuously evicted; dir removed at handoff |
| Assembled testnet list wrong between anchors | Med | §7.4: spaced-checkpoint anchoring + explorer spot-checks; wrong intermediates can only stall (NotFound), never be accepted |
| Forged peer heights poison routing | Med | live height raised only by *delivered, hash-matched* blocks; eviction requires a peer ahead; routing bias only, never data injection |

## 14. Explicitly deferred

- **F4 benchmark harness rebuild** (cached seed states keyed by network /
  height / db format version; per-run RocksDB `Checkpoint` hard-link
  snapshots) — the prerequisite for per-era A/B claims.
- **State-write CPU levers** (from the §12 bottleneck measurements):
  in-memory address-balance cache during IBD; selective per-column-family
  compaction; computing the auth-data root in `convert` on the rayon pool
  instead of write Thread 1.
- **Parallel note-commitment subtrees** (maintainer vision, §15.1):
  precompute per-block, per-pool subtree nodes at verify time (the engine's
  verify-and-commit stage, on rayon) keyed to absolute leaf positions
  prefix-summed over the lookahead window, so the state can append whole
  subtrees instead of per-note sequential hashing. Consensus-critical: must
  be bit-identical to sequential appends (roots **and** serialized
  frontiers), proven by real-vector tests and proptests over arbitrary start
  positions. Staged: (1) zebra-chain precompute + `append_precomputed` +
  equivalence proofs; (2) engine position threading + the thin verify-stage
  wiring; (3) state-side subtree appends on top of the redesigned write
  pipeline. **Status: a stage-1 design/implementation workflow was launched
  and then paused to preserve tokens** — resume from the saved
  `note-commitment-subtrees` workflow script (run `wf_cca64ee5-851`), which
  carries the full brief: two design angles (incrementalmerkletree-native vs
  a recorded shardtree evaluation), judge synthesis, isolated-worktree
  implementation, and equivalence-breaking review.
- **WAL auto-mode and flush-interval tuning** (maintainer question, WAL
  review thread): `disable_wal_during_ibd` only pays off when disk write
  bandwidth is the constraint (it halves write volume by not writing every
  batch twice); on fast disks the sync is CPU-bound and the WAL is nearly
  free, so it stays opt-in. The periodic crash-bound flush is already
  self-tuning in the right direction — in heavy-write eras (spam) the 256 MB
  memtables auto-flush faster than the 5-minute timer, so the timer adds
  ~zero extra I/O exactly when I/O is scarce; it only fires in light-write
  eras, where the L0 files it creates are small and redoing lost work is
  cheapest. Possible follow-ups, deferred until measurements justify them:
  a configurable/adaptive flush interval (effectively `min(bytes, time)` —
  the bytes half already exists as the memtable size), and an auto mode that
  toggles WAL skipping from live flush-stall/disk-utilization tracking —
  rejected for now because it makes crash-durability semantics time-varying
  for a benefit that exists only on slow disks the operator can identify
  statically.
- **Engine extraction into `zebra-sync`** (maintainer-directed): once the
  in-flight redesign workflows land (state write pipeline, generic engine
  unification), move the sync engine out of `zebrad/src/components/ibd/`
  into a new `zebra-sync` workspace crate, so the unified
  checkpoint/full-validation engine has its own API surface, tests, and
  dependency boundary (`zebra-chain`/`zebra-network`/`zebra-state`
  consumers only, no `zebrad` internals).
- Checkpoint-sync disk I/O reduction, e.g. created-then-spent UTXO elision
  (§15.2) — very late phase, **separate PR**.
- Engine property tests rebuilt against the as-built API (the pre-rework
  `engine_prop.rs` was deleted with the reservation machinery it tested).
- Verified self-download of list chunks + cache write-ahead (config fields
  land with the implementations, §10).
- Misbehavior reporting through the address book updater
  (`TODO(known-hash-ibd D6)` sites in the engine), and the `MorePeers`
  stall-ladder step.
- Legacy-syncer tail tuning; connection `Handler` partial-on-timeout change
  (separable network PR); next-gen transport/state-chunk distribution,
  PoW-skip beyond the pinned list, assumeutxo-style snapshots.
- Moving the asset search-directory policy (and its config-naming error
  text) from zebra-chain into zebrad — an altitude question flagged in
  review: the lowest, sync-only library crate currently knows zebrad's
  install layout.

## 15. Design evolution log

This section records how the approved design changed during implementation
and review, so reviewers can distinguish "designed this way" from "changed
with cause". The main spec body above always describes the as-built system.

### 15.1 Resolved by maintainer direction

- **Single-task engine**: one task, one `FuturesUnordered`, no internal
  channels — §4.1. Per-block work is **staged futures** (fetch stage, then
  an engine-pushed verify+commit stage): the stage boundary is what makes
  the commit-pipeline caps and slot retention implementable; a monolithic
  fetch→verify→commit future cannot be paused or observed mid-flight.
- **`Engine` is a future, not a `Stream`**: its natural contract is
  `async fn run() -> IbdOutcome` — it owns scheduling and has no per-item
  consumer. A hand-written `poll_next` over multiple sources + timers is
  the known waker-bug class; the `async fn` + `select!` shape makes the
  push-without-repoll footgun structurally impossible.
- **Arrival-time placement** (rework of the original issuance-time tier
  assignment): the original design reserved size-hint bytes at issuance,
  reconciled on arrival, and published a "memory-eligible boundary" watch
  for an in-flight tier re-check. All of that machinery was deleted in
  favor of one rule — *place the block when it arrives, against the live
  RAM counter* (§4.5) — with size hints keeping exactly one job: batch
  weight packing. Same bounds, one accounting point, far less code.
- **Active span** (rework of the fixed preallocated ring): the window is
  not a fixed-capacity ring sized from config; it grows and shrinks with
  the active span, bounded by the byte budgets, the
  `IBD_WINDOW_MAX_BLOCKS` operational cap (added after measuring O(window)
  rescan cost at high peer counts), and a defensive `IBD_SPAN_MAX` ceiling
  that is bookkeeping insurance, not policy. Slots are not a memory unit;
  blocks are.
- **Disk overflow tier**: generalized from an exception-path spill into the
  window's second tier (§4.5); blocks written **raw** and verified exactly
  once, at promotion (verify-before-write was double work — cached bytes
  are untrusted on load regardless); the stored source address preserves
  attribution.
- **Byte-budget lookahead**: the config exposes only
  `known_hash_lookahead_bytes`; the block-count lookahead emerges (§4.1),
  replacing the legacy block-count `checkpoint_verify_concurrency_limit`
  semantics for the IBD phase.
- **Windowed chunk residency** with verify-all-at-open-then-drop — §6.4.
- **No-mutex rule**: the disk cache was reworked from `Arc<Mutex<_>>` to a
  single-owner struct called synchronously from the engine loop (page-cache
  -bound I/O; see the cache module docs).
- **Constant floor instead of a floor watch**: an earlier E3 design
  published the gate floor on a `watch<Option<Height>>` with a drop-guard
  (`None` on engine exit). Replaced by a startup-computed constant `Height`
  (§7.2) — first computed inside `router::init` from the config flag, then
  computed once in `zebrad`'s startup wiring and passed in, removing
  consensus's knowledge of the list entirely. Finally (PR review,
  maintainer-directed) the list-max raise was dropped altogether: the gate
  became the stateless `checkpoints::CheckpointGateLayer` fixed at the
  mandatory checkpoint height, the verifier processes anything above
  Canopy, and protecting the engine's range above Canopy became the
  callers' responsibility (§7.2's residual-responsibility note). Accepted
  consequences: the Degraded-handoff question in §7.2, and below-Canopy
  `submit_block` duplicates reporting `Rejected` (the stateless gate fires
  before the duplicate check).
- **Per-height peer exclusions: deleted.** The design specified per-height
  exclusion sets steering refetches away from `notfound`/bad-copy peers.
  As built, routing is owned by zebra-network's peer set, which the engine
  cannot steer per-request — the exclusion records had no consumer, so the
  machinery (a `peer_stats` module with rotation timers) was write-only
  dead weight and was removed in the cleanup wave. What remains: `notfound`
  counting (metric + per-slot counters feeding backoff and stall logs), and
  the inventory registry's own missing-marks for single-hash hedges. The
  right future fix is at the network layer (registry-aware multi-hash
  routing or an avoid-list in the request), recorded in §14 alongside the
  D6 misbehavior-reporting TODO.
- **Review wave highlights** (each verified, fixed, and tested): the NU5+
  auth-data check was missing from the pipeline path (restored on write
  Thread 1, §2); the frontier could deadlock against the commit-pipeline
  caps (frontier bypass added); the batch layer's concurrency was frozen at
  a startup snapshot of zero ready peers (now built at the 96 ceiling with
  the live issuance gate) — this one fix took early-sync throughput from
  ~40 to ~400 blocks/s; tip-stall eviction fired on synced/quiet/regtest
  nodes (peer-ahead gate added); `Arc::try_unwrap` never succeeded because
  the non-finalized state was published per-block (publication suppressed
  during the pipeline, enabling in-place chain mutation).

### 15.2 Checkpoint-sync disk I/O reduction (very late phase, separate PR)

During the known-hash phase the write thread persists UTXOs that a later
block in the same sync window provably spends — a create-then-delete pair in
RocksDB that compaction must later clean up. The spam-attack era is the
worst case: enormous volumes of dust outputs created and spent within short
spans.

**Key enabler — zero-hash future-spend harvesting.** The disk overflow tier
(§4.5) holds many blocks ahead of the commit frontier, and a transaction's
inputs name the txids they spend *explicitly* — so the set of outpoints
spent in the next H blocks can be harvested by mere iteration over the
already-deserialized blocks, with **no transaction hashing**. On the other
side of the query, the committing block's own txids are already computed for
its state indexes — so "is this freshly-created output spent within the
horizon?" is a free set-membership test on both ends. During the known-hash
phase this knowledge is *certain*, not speculative: the list pins the chain.
Memory bound: a bloom/cuckoo filter over the horizon's outpoints (~10
bits/entry) — a false positive merely skips an elision, never breaks
correctness.

Candidate uses, in ascending risk order:

1. **Spent-UTXO read-ahead**: prefetch/warm the UTXO records the write
   thread will need — no consensus-path change at all.
2. **Within-batch elision**: accumulate multiple blocks into one atomic
   RocksDB write batch and elide create+spend pairs that fall entirely
   inside it — crash-safe by construction.
3. **Cross-batch elision**: the largest win in the spam era, but it needs
   explicit crash-consistency (a persisted deferral record or recovery-time
   re-derivation).

Caveats recorded now: address-index entries (balances, tx-by-address
history) must still be written — only the UTXO column-family pair elides;
interaction with the F2 compaction pause and the two-thread pipeline must be
measured, not assumed; and any change here is behavior-sensitive on the
finalized write path, so it ships as its own PR with its own review.

## 16. State snapshot at H_max (assumeUTXO-style) — proposed next step

Status: **proposed**, not implemented. This supersedes the within-batch /
cross-batch UTXO elision sketch (§15.2): with a snapshot the node has *certain
foreknowledge* of the end state, so there is no batch window and no crash-lag,
and the entire `0..=H_max` range is skipped rather than replayed. It is pursued
only after the current known-hash engine + lists are accepted.

### 16.1 Idea

Ship, as SHA-256-pinned release assets alongside the known-hash list, the
checkpoint-range *result* at `H_max` (= the known-hash list max height — the
height the node already trusts), and load it once, atomically, before the
known-hash engine starts. The node then downloads block/tx data and validates
normally *past* `H_max`, never re-deriving the chain state for `0..=H_max`. Two
artifacts (two halves of one feature):

- **Note commitment trees at `H_max`** — Sapling/Orchard/Sprout tip frontiers,
  the complete historical anchor sets, subtree roots, the history tree, and the
  complete nullifier sets. Loading these eliminates the irreducibly-sequential
  Sinsemilla/Pedersen `update_trees_parallel` frontier appends — the dominant
  CPU cost across the spam range.
- **Survivor transparent outputs at `H_max`** — the outputs created at height
  ≤ `H_max` and still unspent at `H_max`, as full `(OutputLocation → Output)`
  entries, plus the final cumulative `tip_chain_value_pool`, `block_info[H_max]`,
  and `balance_by_transparent_addr`. Identity is the `OutputLocation` (height,
  tx-index, output-index) — 8 bytes, already the `utxo_by_out_loc` key, and the
  survivor set *is* that column family's keyset at `H_max`. Loading these lets
  the node track only survivors and skip the create-then-delete churn of every
  transient dust output entirely.

### 16.2 Why ship survivors (not a filter), and skip the range (not replay it)

The §15.2 bloom/cuckoo filter only helps an approach that *replays* `0..=H_max`
and elides within a window. The snapshot does not replay that range at all, so
re-deriving `Output` bodies would re-introduce the very work being eliminated —
ship full survivor entries and skip `0..=H_max`. The value-pool dependency that
blocked elision (a block's value pool needs every spent output's value, which
panics if a transient was never written) dissolves: no in-range block is
committed, so the final value pool, `block_info`, and balances are shipped
directly instead of derived.

### 16.3 Trust and verification

Same trust root as the known-hash list, content-addressed so mirrors/peers are
untrusted:

- **Chunks**: each snapshot chunk is SHA-256-pinned in a reviewed in-source
  `SnapshotSpec` constant (mirrors `KnownHashListSpec`), verified at load.
- **Trees**: the loaded `history_tree.hash()` / tip roots are checked against
  the `hashFinalSaplingRoot` (pre-NU5) / `hashBlockCommitments` (NU5+) committed
  in the trusted `H_max + 1` known-hash header, via the existing
  `check::block_commitment_is_valid_for_chain_history` machinery.
- **Transparent UTXO set**: Zcash commits no header field to the transparent
  UTXO set, so ship one reviewed per-network UTXO-set hash and recompute it over
  the loaded set at load.
- **Optional background full-validation** (assumeUTXO-equivalent): replay
  `0..=H_max` from the pinned blocks to confirm the loaded end state.

### 16.4 Staged plan

- **Stage 1 (minimal, measurable): shielded-only snapshot.** Load the
  trees/anchors/subtrees/sprout/history/nullifiers at `H_max` in one atomic
  batch before engine start; leave the transparent write path untouched
  (`0..=H_max` still committed transparently). Smallest correctness surface (no
  value-pool/survivor/address-index dependency), cleanest verification, and it
  removes the dominant CPU bottleneck on its own.
- **Stage 2**: `SnapshotSpec` + windowed verify-at-open loader in `zebra-chain`
  (mirrors `known_hashes.rs`).
- **Stage 3**: emitter — extend the `known-hashes` `zebra-utils` tool with a
  snapshot-emit mode that opens a synced node's RocksDB **read-only** (RPC
  cannot dump the UTXO/anchor/nullifier/sprout/history CFs) and writes sorted,
  SHA-pinned chunks.
- **Stage 4**: transparent survivor set + value pool + balances, via a
  direct-batch bulk loader in `FinalizedState` (sibling to `write_block`, *not*
  `commit_finalized_direct`).
- **Stage 5**: wire a one-shot install into `IbdEngine::run` before engine
  construction, gated on `known_hash_sync` + a verified snapshot + an
  empty/below-`H_max` tip; atomic batch with compaction paused (the tip flips to
  `H_max` only on full commit, so a crash mid-load leaves the prior tip).
- **Stage 6 (deferred)**: background full-validation + lazy backfill of
  `tx_loc_by_transparent_addr_loc` and sub-`H_max` historical-treestate RPC.

### 16.5 Distribution over P2P (further future)

Instead of (or besides) shipping the chunks as release assets, a node can
**fetch them from other Zebra peers and verify them content-addressed** against
the same SHA-pinned digests / header-committed roots — so the binary carries
only the digests and the repo need not vendor the `.bin` weight. This needs a
custom Zebra-to-Zebra wire message (the Zcash protocol has none), negotiated via
a user-agent marker / reserved service bit so zcashd peers are unaffected (the
codec already drops unknown commands), ranged into ≤1 MiB sub-chunks (the 2 MiB
`MAX_PROTOCOL_MESSAGE_LEN` frame bound), rate-limited and DoS-bounded like all
inbound requests, advertised + routed via the inventory registry, with a
bundled-asset / download-URL fallback while few peers serve it. Verification is
unchanged from §16.3; only the byte source moves to the network.

### 16.6 Measurement plan

The reason to build Stages 1 + 4 first is to **measure**: full-sync wall time
*with* the snapshot (pre-prepared trees + survivor outputs at `H_max`) vs. the
current known-hash engine baseline, on the same hardware, for both Mainnet and
Testnet. Measurement artifacts are generated locally from a synced state rolled
back / stopped at `H_max`; no P2P distribution is needed to measure.
