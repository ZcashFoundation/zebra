# Known-Hash Initial Block Download Engine

- Status: design approved by maintainer (2026-06-11); implementation in progress
- Base: `main` @ `80709f096` (v5.1.0)
- Branch: `ibd-engine`
- Tracking issue: TBD

This document is the review artifact for the `ibd-engine` branch. Each phase below
maps to specific commits; reviewers should check each commit against the spec
section it implements rather than re-deriving the design from the diff.

## 1. Motivation

Zebra's initial sync discovers block hashes from peers (`obtain_tips`/`extend_tips`
FindBlocks rounds), downloads blocks one hash per request, funnels every block
through the `CheckpointVerifier`, and commits in-order on a single state write
thread. With a bundled every-block checkpoint list (3,358,432 mainnet hashes), the
discovery step is unnecessary for ~99% of IBD — and once hashes are known in
advance, most of the remaining architecture is wrong by construction:

1. **Discovery latency**: each FindBlocks round costs a network RTT plus hundreds
   of serial state reads, for hashes we already know (#10192).
2. **Serial verification floor**: all checkpoint CPU work (equihash, merkle root,
   transaction hashing) runs inside the consensus router's single `tower::Buffer`
   worker — one core, regardless of download parallelism.
3. **Restart churn**: unrecoverable errors trigger `cancel_all()` + a 67s restart
   delay, frequently with thousands of downloads still in flight (#10192, #10438).
4. **Gap stalls**: a single missing block at the in-order commit frontier stalls
   the pipeline while later blocks pile up; nothing prioritizes refetching it.
5. **Startup waste**: the consensus router's background task walks the *entire*
   checkpoint list with one serial state request per entry on every restart — with
   an every-block list, up to ~3.36M requests through the write-path state buffer.

This branch replaces the checkpoint phase end-to-end with a new **known-hash IBD
engine**: maximally parallel download and verification of blocks whose hashes are
pinned by a bundled, SHA-256-verified list, feeding the state in strict height
order. The `CheckpointVerifier` is deleted at the end of the branch. The legacy
syncer (`obtain_tips`/`extend_tips`) remains for the post-IBD tail and steady-state
tip-following.

## 2. Trust and integrity model

The trust root is unchanged from today's checkpoint model: hard-coded SHA-256
constants for the bundled hash-list chunks are reviewed at PR time, exactly like
the existing `main-checkpoints.txt` entries. Runtime verification proves
*file == pinned hash*; human review proves *pinned hash == canonical chain*. The
loader cross-checks `list[0] == network.genesis_hash()` and rejects misaligned or
empty chunks.

What replaces the deleted checks:

| Check | Old location | New location | Justification |
|---|---|---|---|
| Block hash == expected hash | CheckpointVerifier hash-chain walk | `connection.rs` `Handler::process_message` (`pending_hashes.remove(&block.hash())`, covers multi-hash requests) + engine assigns hashes from `list[height]` | Header double-SHA256 recomputed on receipt; non-matching blocks ignored |
| Parent-hash linkage | CheckpointVerifier range walk | List pins it transitively; engine pre-send check `block.header.previous_block_hash == list[h-1]` converts list bugs from state-assert panics into fatal diagnostics; state asserts retained | Defense in depth |
| Coinbase height | `check_block` | Engine convert step: `coinbase_height() == Some(assigned_height)` | Pinned by merkle root |
| Difficulty threshold + check | `checkpoint.rs` | **Dropped** | `nBits` is in the pinned header |
| Equihash solution | `checkpoint.rs` | **Dropped** | `nSolution` is in the pinned header; re-verifying could only reject a block the list already commits to |
| Merkle root vs txids | `checkpoint.rs` via `merkle_root_validity` | Engine convert step, **same function** (`zebra_consensus::block::check::merkle_root_validity`) | **Must be retained**: the block hash pins only the header; this check extends the pin to transaction bodies. Tx hashes are computed anyway for the state request, so it is nearly free |
| Duplicate-txid (CVE-2012-2459) | inside `merkle_root_validity` | retained automatically (same call) | Reuse, never re-implement |
| Tx branch-ID consistency | inside `merkle_root_validity` | retained automatically | |
| NU5+ auth-data commitment | state `commit_finalized_direct` (never in CheckpointVerifier) | unchanged | v5 txids do not commit to witnesses; substituted auth data is caught at commit time → engine refetches from a different peer with attribution (§4.6) |
| ≤4 queued blocks per height | `MAX_QUEUED_BLOCKS_PER_HEIGHT` | engine reorder buffer holds exactly one block per height, byte-capped | Strictly stronger |
| Mandatory checkpoint floor (Canopy) | router `init_checkpoint_list` | router commit gate (§7.2): semantic commits below `mandatory_checkpoint_height` rejected unconditionally | Stronger: a constant of the network, not a function of config |
| State-vs-list consistency on restart | router background full-list walk | O(1) spot-check: `BestChainBlockHash(tip)` vs `list[tip]` + genesis | Fixes the 3.36M-request startup walk |

Consensus-acceptance change: **none**. Every block the engine commits is pinned
byte-for-byte (header by hash, body by merkle root, v5 auth data by the existing
state-side `hashBlockCommitments` check). Checks that were never run on the
checkpoint path (signatures, UTXO validity, subsidy, time) are still not run below
the final known height.

## 3. Architecture overview

The engine lives in `zebrad/src/components/ibd/` and talks only to zebra-network
(batched `Request::BlocksByHash`) and zebra-state
(`Request::CommitCheckpointVerifiedBlock`). No zebra-consensus *service* is on
the path; two pure zebra-consensus helpers are reused:
`primitives::spawn_fifo` (rayon FIFO + oneshot, `primitives.rs:37`) and
`block::check::merkle_root_validity`.

```
                              zebrad ibd engine — ONE task, no internal channels
                    ┌──────────────────────────────────────────────────────────────┐
                    │ ENGINE (engine.rs)                                           │
 zebra-network      │  window: VecDeque<Slot> ring, with_capacity from config,     │
┌────────────┐      │          preallocated at startup, index = height - base      │
│  PeerSet   │      │  blocks: FuturesUnordered<BlockFut> — one future per block,  │
│ (Buffer'd) │      │          issued in height order while under caps             │
└─────▲──────┘      │  policies: refill, overflow fetch-ahead, gap hedging, stalls │
      │             └───────┬──────────────────────────────────────────────────────┘
      │ one BlocksByHash    │ per-block future: fetch → verify → commit
      │ per flushed batch   ▼
┌─────┴────────────────────────────────┐      ┌──────────────────────────────────┐
│ BATCHED FETCH (fetch.rs)             │      │ VERIFY-AND-COMMIT (convert.rs)   │
│ tower-batch-control over a          ─┼──────► spawn_fifo(convert) on rayon:    │
│ FetchBatcher inner service;          │      │ tx hashing + merkle + linkage,   │
│ weight = list size hint (§6);        │      │ then CommitCheckpointVerified-   │
│ flush at 16 items / ≥800 KB weight   │      │ Block; future resolves after the │
│ / flush latency. Gap hedges bypass   │      │ RocksDB write                    │
│ batching via route_inv (single hash) │      └───────────────┬──────────────────┘
└──────────────────────────────────────┘                      │
                            cache.rs ◄── disk overflow tier   ▼
                            (disk, §4.5)              ┌──────────────┐
                                                      │ StateService │
                                                      │ (Buffer'd) → │
                                                      │ write thread │
                                                      └──────────────┘
```

Ownership rules: one owner per piece of state, all cross-task communication via
mpsc/watch channels, no shared mutexes. The supervisor (`ibd.rs`) owns all task
handles and a `watch<bool>` run flag; shutdown is `JoinSet::abort_all()` plus
channel teardown.

### 3.1 Module layout

```
zebrad/src/components/ibd.rs                 IbdEngine, IbdOutcome, supervisor restart loop, handoff
zebrad/src/components/ibd/
├── engine.rs               single engine task: ring window, per-block futures, refill,
│                           tiered fetch-ahead, gap hedging, stall ladder
├── fetch.rs                FetchRequest (RequestWeight impl) + FetchBatcher inner service
│                           for tower-batch-control; NotFound/transport classification;
│                           hedge path (route_inv, bypasses batching)
├── convert.rs              convert() + VerifyAndCommit tower service over the state
├── cache.rs                disk overflow tier: download each block at most once
├── peer_stats.rs           per-peer delivered-blocks/latency accounting, exclusion sets
└── tests/                  vectors.rs, engine_prop.rs (window/pipeline invariants)

zebra-chain/src/parameters/known_hashes.rs   KnownHashListSpec + windowed chunk loader (§6)
zebra-chain/src/parameters/known_hashes/     *.bin chunk assets (not packaged to crates.io)
```

(Module layout convention: `foo.rs` + `foo/` directory, never `foo/mod.rs`.
`BlockSizeTracker` from syncer-performance is no longer lifted — the list's
per-block size hints (§6) supersede adaptive batch sizing.)

### 3.2 Named constants

All overridable via config (§10); values are provisional pending sync_perf harness
measurements and recorded here so reviewers can check the code against the spec.

| Constant | Value | Rationale |
|---|---|---|
| `IBD_BATCH_MAX_BLOCKS` | 16 | wire serving limit (`GETDATA_MAX_BLOCK_COUNT`); overflow is **silently dropped** by serving nodes |
| `IBD_BATCH_MAX_WEIGHT` | 800,000 | tower-batch-control `max_items_weight_in_batch` — a flush-after-crossing threshold (`worker.rs:149→253`): all-but-the-last item sum to < 800 KB; serving stays safe per the §4.2 serve-after-one analysis |
| `IBD_BATCH_FLUSH_LATENCY` | 10 ms | tower-batch-control `max_latency`: fills batches without adding meaningful per-block latency |
| `SIZE_HINT_UNIT` | `MAX_BLOCK_BYTES.div_ceil(255)` = 7,844 B | size-hint quantum: hint `w` (1..=255) means serialized size ≤ `w × SIZE_HINT_UNIT` |
| `IBD_LOOKAHEAD_BYTES` | 256 MiB (config `known_hash_lookahead_bytes`) | **the checkpoint lookahead limit, as a byte budget** — the block-count lookahead is not configured; it auto-scales from the list's per-block size hints (§4.1) |
| `IBD_SPAN_MAX` | 65,536 | hard cap on the window's height span (ring capacity, allocated once at startup); bounds slot metadata and the auto-scaled lookahead regardless of how small blocks are |
| `IBD_MAX_CONCURRENT_BATCHES` | min(3 × ready_peers, 96) | tower-batch-control `max_concurrent_batches`: ~3× oversubscription keeps every freed connection instantly busy |
| `IBD_COMMIT_PIPELINE_BLOCKS` | 1,024 | max unresolved commit futures (the only real state backpressure signal) |
| `IBD_COMMIT_PIPELINE_BYTES` | 64 MiB | byte bound on the same |
| `IBD_FRONTIER_CRITICAL_SPAN` | 64 | heights from the commit frontier treated as gap-critical |
| `IBD_GAP_HEDGE_AFTER` | 5 s | age of a frontier-critical in-flight slot before a hedged single-hash refetch |
| `IBD_HEIGHT_RETRY_BACKOFF` | 500 ms × 2^attempts, cap 30 s | per-height retry pacing |
| `IBD_RESTART_DELAY` | 15 s | supervisor delay between engine restarts |
| `IBD_STALL_WARN_INTERVAL` | 60 s | frontier-stall warning cadence |
| `COMMIT_FAILURE_ATTEMPT_LIMIT` | 3 | deterministic-failure detector (§4.6) |

### 3.3 Memory-bound audit (DoS review anchor)

Every allocation on the engine path, its bound, and how the bound is verified
after implementation. The design rule is: **block bytes live in exactly one
accounted place at a time**, and every container is bounded by bytes and count
(blocks range ~1.6 KB – 2 MB; the 2 MB protocol cap is enforced at deserialization
in zebra-network, so no single response item can exceed it).

| Structure | Bound | Enforced by | Verified by |
|---|---|---|---|
| In-flight batch responses (network + batch-control queue) | `IBD_MAX_CONCURRENT_BATCHES` × (800 KB + one crossing block ≤ ~2 MB) ≈ 269 MB worst case, realistically ≪ (spam-era batches are single-block); concurrent-batch count is also the **only** byte bound on disk-tier fetches — the lookahead budget covers the memory tier only | engine issuance caps + tower-batch-control queue limits | engine property tests + worst-case accounting test |
| In-flight fetch bytes | **list size hints — a priori upper bounds** (§6.2), reconciled to exact on arrival | hints round up, so the budget can only over-reserve, never under-count (no estimate drift) | property test against real per-era size distributions |
| Engine window (`Slot::Fetched` blocks; lookahead) | `IBD_LOOKAHEAD_BYTES` (256 MiB, config) + `IBD_SPAN_MAX` count cap; block-count lookahead auto-scales from size hints (§4.1); same `Arc` as the state queue holds — counted once | byte budget: hint upper bounds until arrival, exact after | `ibd.window.bytes` + `ibd.lookahead.blocks` gauges + gate 4; property test |
| Verify-commit pipeline (rayon queue + state queue + write channel, jointly) | `IBD_COMMIT_PIPELINE_BLOCKS` (1,024) / `IBD_COMMIT_PIPELINE_BYTES` (64 MiB) unresolved futures | engine issuance caps — futures resolve only after the disk write | `engine_prop.rs` exact-accounting fuzz |
| State `finalized_state_queued_blocks` + unbounded write channel | transitively ≤ the commit-pipeline caps: **only the engine submits during IBD** (the §7.2 gate excludes gossip/RPC below the floor) | engine caps + gate | mid-IBD injection test (E3); state queue-len metric |
| Window slot metadata (ring) | `IBD_SPAN_MAX` (65,536) × ~40 B ≈ 2.6 MB; `VecDeque::with_capacity` preallocated once at startup | span-cap invariant (no growth; applies even when the disk cache extends fetch-ahead) | property test + `debug_assert` on push |
| Disk overflow tier | disk not RAM; ≤ `IBD_SPAN_MAX` blocks (transient double-storage of blocks the state will store anyway; no byte cap, no config); in-memory index ≤ ring capacity | span cap; continuous eviction on commit | `ibd.cache.bytes` gauge; kill/restart test |
| Per-height exclusion sets, peer stats, hedge dedup | O(span) × SmallVec[4] and O(peers) — KBs | ring capacity / peer-set size; cleared on rotation / frontier advance | code review (no block data stored) |
| Known-hash list + size hints | **1–2 resident chunks (~5–10 MB) + ~3.4 MB hints during IBD; 0 after the tip passes `max_height`** (§6.4) | windowed loader; drop-at-handoff | RSS gate 4; handoff RSS assertion |

Unsolicited or duplicate peer data never enters these structures: the connection
`Handler` drops blocks that don't match a requested hash, and hedge duplicates
are dropped at the single per-height window slot. The state's queues are
unbounded *types*, but during IBD their only writer is the engine; the
pre-existing non-finalized queue behavior for gossiped blocks above the floor is
unchanged from today. Final assurance is empirical, per the maintainer: gate 4
(§11) pins the process RSS ceiling, and each row above maps to a gauge or
property test that lands with its commit.

## 4. Component specifications

### 4.1 Engine task (`engine.rs`)

One task owns everything: a fixed-capacity ring window and one
`FuturesUnordered` of per-block stage futures. There are no internal data
channels and no second stateful task — one byte-accounting point. (One control
signal exists: a `watch` publishing the memory-eligible boundary, read by
in-flight fetches for the tier re-check below.) (This is the
single-task consolidation the maintainer approved, refined with the ring buffer
and the unified per-block futures.)

```rust
enum Slot {
    Unrequested { attempts: u8, not_founds: u8, backoff_until: Option<Instant> },
    InFlight    { attempts: u8, since: Instant, hedged: Option<AbortHandle>,
                  abort: AbortHandle },     // its in-flight future(s)
    Fetched     { block: Arc<Block>, bytes: u32 },  // fetch stage done; commit
                                                    // continuation pending or in flight;
                                                    // retained until commit resolves for
                                                    // §4.6 reset recovery (same Arc the
                                                    // state queue holds — no extra memory)
    Cached      { bytes: u32 },   // in the disk tier (§4.5); never re-requested
}

struct Engine {
    window: VecDeque<Slot>,       // std's ring buffer: with_capacity(IBD_SPAN_MAX) from
                                  // config at startup; never grows (span-cap invariant +
                                  // debug_assert); index = height - base, O(1)
    base: Height,                 // lowest uncommitted height; pop_front on advance
    blocks: FuturesUnordered<BlockFut>,  // per-block STAGE futures (see below)
    // services: batched_fetch (§4.2), verify_and_commit (§4.3), cache, peer_stats
}
```

Per-block work is **staged futures in one collection** — still one task, one
`FuturesUnordered`, no internal channels, but the engine observes the
fetch→commit boundary (this is load-bearing: it is how the commit-pipeline
caps are enforced and how `Slot::Fetched` retention works):

```rust
// Stage 1 — fetch (every block). Resolves when the block arrives:
async fn fetch_stage(req: FetchRequest, ...) -> (Height, FetchOutcome) {
    let (block, source) = batched_fetch.oneshot(req).await?;   // §4.2, weighted batching
    // Tier re-check at completion (the frontier may have advanced mid-fetch):
    if height <= memory_eligible_rx.borrow().0 {
        FetchOutcome::Fetched { block, source }    // engine pushes stage 2
    } else {
        cache.put(height, block, source).await;    // §4.5: save RAW to disk —
        FetchOutcome::Saved { bytes }              // verification runs once, at
    }                                              // promotion; skipping it here
}                                                  // avoids double tx-hashing
                                                   // (worst in the spam era)

// Stage 2 — verify+commit (memory-tier blocks and promotions). Pushed by the
// engine on stage-1 completion (or cache promotion), ONLY while unresolved
// stage-2 futures are under IBD_COMMIT_PIPELINE_BLOCKS / _BYTES:
async fn commit_stage(height: Height, block: Arc<Block>, source: ...) -> (Height, CommitOutcome) {
    verify_and_commit.oneshot(IbdBlock { height, block, source }).await  // §4.3
}
```

On a stage-1 completion the engine reconciles hint→exact bytes, sets
`Slot::Fetched`, and pushes the stage-2 continuation if the commit pipeline has
room — otherwise the block waits in its slot (that wait is the concrete
mechanism behind §4.4's cap enforcement). A `Tier::Disk` fetch that saved to
disk just flips its slot to `Cached`. The tier re-check inside stage 1 means a
height issued for the disk tier that has *become* frontier-critical mid-fetch
commits directly instead of round-tripping through disk.

Main loop: `select!` over `blocks.next()` (completion: update the slot, advance
`base` past committed heights, evict cache entries) and a refill step:

1. **Refill, lowest-missing-first, with auto-scaled lookahead**: the lookahead
   block count is not configured — it emerges from the byte budget and the
   list's per-block size hints. For the next needed height: a `Cached` slot is
   **promoted** (read from disk through the normal §4.3 verify-and-commit
   path); an `Unrequested` slot with expired backoff gets a stage-1
   fetch future — both while
   `budget_used + hint_upper(next) ≤ IBD_LOOKAHEAD_BYTES` and the
   commit-pipeline caps (§4.4) hold. `budget_used` counts hint **upper bounds**
   for blocks not yet arrived and exact serialized sizes once arrived — hints
   round up, so every arrival can only free budget, ratcheting the lookahead
   head forward. The memory window self-tunes per era with zero configuration:
   small-block eras look ahead tens of thousands of blocks (span-capped), the
   sandblasted era a few hundred. Batch formation is not the engine's job:
   contiguous height-ordered issuance into the weighted batch layer (§4.2)
   yields contiguous, size-fitted batches by construction.
2. **Overflow fetch-ahead (general, not a stall special case)**: once the
   memory budget is fully reserved, keep issuing `Tier::Disk` futures for
   further heights while the ring span (`IBD_SPAN_MAX`) allows — the network
   stays busy whenever it is faster than
   the state, and the downloaded chain accumulates verified on disk instead of
   in memory (§4.5).
3. **Gap hedging**: any height in `[base, base + IBD_FRONTIER_CRITICAL_SPAN)`
   still `InFlight` after `IBD_GAP_HEDGE_AFTER` gets a second, **single-hash**
   future via the direct `route_inv()` path (bypasses batching): inventory-aware
   routing for free — advertising peers preferred, known-missing peers skipped,
   and the one-request-per-connection rule lands the hedge on a different peer
   by construction. First completion wins; the slot's `AbortHandle` cancels the
   loser.

Retry policy is owned by the engine loop (futures return outcomes; they never
retry internally beyond §4.2's one explicit-notfound round). Per-height attempt
counts and per-height peer-exclusion sets
(`excluded_peers: HashMap<Height, SmallVec<PeerSocketAddr>>`) live here;
exclusion sets are cleared on the inventory-rotation cadence (53 s) so recovered
peers are retried.

**Stall escalation ladder** (replaces ibd-minimal's one-shot fallback — the engine
owns its range and never permanently abandons it):

1. normal assignment → 2. hedge after 5 s → 3. reassign on each NotFound/timeout
(exclusion only on explicit NotFound) → 4. after 8 attempts, back off 10 s between
attempts and send `MorePeers` demand → 5. after 60 s stalled, `warn!` with
height/attempts/peer-count + `ibd.stall.seconds` metric, repeating every interval.

Degradation policy (split at the mandatory checkpoint): below
`mandatory_checkpoint_height` (Canopy − 1; 1,046,399 on mainnet) the engine loops
forever with alarms — semantic sync below Canopy is not a sound fallback. Above
it, after 5 consecutive supervisor restarts with zero frontier progress, the
engine may return `Degraded` and let the legacy syncer take over (correct, just
slower), behind an explicit `warn!`.

### 4.2 Batched fetch service (`fetch.rs`)

Batching is a **tower-batch-control** layer (already weight-aware on main:
`RequestWeight` trait at `tower-batch-control/src/lib.rs:124-131`,
`max_items_weight_in_batch` + `pending_items_weight` in `worker.rs`, plus a
`max_latency` flush) over a `FetchBatcher` inner service — the same
`Service<BatchControl<R>>` pattern as the batch crypto verifiers, and each
`Item` call returns its own response future, so per-block response distribution
is native:

- `FetchRequest { height, hash, size_hint }` implements `RequestWeight` by
  returning the list's size-hint upper bound in bytes (§6). Batch parameters:
  `max_items_in_batch = IBD_BATCH_MAX_BLOCKS` (16),
  `max_items_weight_in_batch = IBD_BATCH_MAX_WEIGHT` (800 KB),
  `max_concurrent_batches = IBD_MAX_CONCURRENT_BATCHES`,
  `max_latency = IBD_BATCH_FLUSH_LATENCY` (10 ms). **Overflow analysis
  (verified)**: the worker admits the crossing item *before* flushing
  (`worker.rs:149` adds weight and dispatches, `:253` checks the cap), so a
  flushed batch's hint weight is < 800 KB **plus at most one block** (worst
  ≈ 2.8 MB). This is still served in full: zebrad's serving loop includes at
  least one block before applying the 1 MB limit and checks *before* each
  subsequent block (`inbound.rs:431-436`) — since every prefix of an
  honest batch except the final block sums to < 800 KB < 1 MB, no block is
  ever trimmed; zcashd's send-buffer throttling defers rather than drops.
  The 800 KB threshold leaves a 200 KB margin under the serving limit for
  hint quantization.
- `FetchBatcher` accumulates `Item(FetchRequest)`s (stashing each item's
  responder); on `Flush` it issues one
  `network.ready().await?.call(BlocksByHash(batch))` through the existing
  Buffer'd peer set (no tower Hedge/Retry stack; hedging is the engine's
  smarter job; the 20 s `REQUEST_TIMEOUT` bounds each round), then matches
  `Available` blocks to items by hash and fulfills each responder with
  `(block, source_addr)`.
- Since the engine issues requests in height order, flushed batches are
  contiguous height runs by construction (modulo interleaved retries — fine:
  every height is deep history, servable by any peer).

**Response semantics (verified on main)**: a timeout fails the *whole* request
(accumulated blocks are discarded by the connection handler) — classified as
transport loss for every item in the batch, with no peer penalty beyond EWMA
load. Partial responses exist only when the peer sends explicit `notfound`
(zebrad does, zcashd doesn't): `Missing` entries are classified NotFound
(per-height peer exclusion + re-issue), `Available` entries are used. One
immediate re-request round inside `FetchBatcher` is allowed for the missing
remainder of an explicit-notfound partial; everything else resolves the item
future with a classified error so the engine's retry policy (priority, backoff,
hedging) decides.

**Inventory-registry contract (#10679/#5709)**: the engine never writes to the
registry. Marking stays in the client layer (`MissingInventoryCollector`), which
only forwards explicit `Missing`/`NotFoundResponse` — transport errors never
poison routing. The engine's own exclusion sets are local, per-height, and
time-bounded. Error classification in `fetch.rs` mirrors the same taxonomy:
explicit NotFound ⇒ "peer lacks block"; timeout/drop/overload ⇒ no exclusion.

### 4.3 Verify-and-commit tower service (`convert.rs`)

Verification is a tower middleware **on top of the buffered state service**,
using zebra's existing rayon bridge
`zebra_consensus::primitives::spawn_fifo` (`primitives.rs:37` — sends a closure
to the rayon FIFO pool and returns the result on a oneshot). One `call()` per
block = verify on rayon, then commit to the state; the returned future resolves
only after the RocksDB write. This removes the single-Buffer-worker verification
floor: conversions run in parallel across the global rayon pool
(`zebrad/src/application.rs:431`, sized by `sync.parallel_cpu_threads`), shared
with the batch-crypto verifiers — which are idle during IBD, so the engine gets
the whole pool, and they share fairly via FIFO near the handoff.

```rust
// pure, runs on rayon
fn convert(network: &Network, height: Height, expected: block::Hash,
           prev_expected: block::Hash, block: Arc<Block>)
           -> Result<(CheckpointVerifiedBlock, usize), ConvertError> {
    if block.coinbase_height() != Some(height)           { return Err(WrongHeight); }
    if block.header.previous_block_hash != prev_expected { return Err(BrokenLink); }
    // tx hashing + new_outputs — the expensive part, parallel across blocks
    let cvb = CheckpointVerifiedBlock::with_hash(block.clone(), expected);
    // header hash pins only the header; extend the pin to tx bodies.
    // Reuses zebra_consensus::block::check::merkle_root_validity VERBATIM —
    // carries the CVE-2012-2459 duplicate-txid check and branch-ID consistency.
    check::merkle_root_validity(network, &block, &cvb.transaction_hashes)?;
    Ok((cvb, block.zcash_serialized_size()))
}

// the tower layer; ZS = Buffer'd state service
impl<ZS> Service<IbdBlock> for VerifyAndCommit<ZS> {
    // poll_ready delegates to the state Buffer; real backpressure is the
    // engine's unresolved-future caps, not readiness.
    fn call(&mut self, IbdBlock { height, block, source }: IbdBlock) -> Self::Future {
        let (expected, prev) = (self.list[height], self.list[height - 1]);
        let state = self.state.clone();  // clone before the async block (tower convention)
        async move {
            let (cvb, bytes) = spawn_fifo(move || convert(&network, height, expected, prev, block))
                .await
                .map_err(|_| ConvertError::RayonShutdown)??;
            state.oneshot(zs::Request::CommitCheckpointVerifiedBlock(cvb)).await
        }.boxed()
    }
}
```

Failure semantics (errors carry `source` for attribution):
`BadMerkleRoot`/`DuplicateTransaction` ⇒ the delivering peer sent a corrupt body
for a real header — misbehavior report for the source peer, slot →
`Unrequested` with immediate refetch from a different peer (the copy never met
the §4.5 invariant condition, so this is not a duplicate download).
`WrongHeight`/`BrokenLink` ⇒ list inconsistency — **fatal diagnostic** naming
height, hash, and chunk index; never a retry loop. Commit-stage errors follow
§4.6.

### 4.4 Commit pipeline and state protection (within the engine task)

There is no separate feeder task: commits are stage-2 futures in the engine's
single collection (§4.1), and the caps are enforced at the stage boundary —
the engine pushes a stage-2 (verify+commit) continuation only while unresolved
stage-2 futures are under `IBD_COMMIT_PIPELINE_BLOCKS` (1,024) /
`IBD_COMMIT_PIPELINE_BYTES` (64 MiB); fetched blocks above the caps wait in
their `Slot::Fetched` entries, and heights beyond the memory budget issue as
`Tier::Disk` into the overflow tier (§4.5).

Why this protects the state: `CommitCheckpointVerifiedBlock` enqueues
non-blocking into the state's **unbounded** parent-hash-keyed queue and write
channel; the only true backpressure signal is the commit-completion future
(which resolves after the disk write). Capping unresolved commits bounds
everything downstream. Ordering: the state's finalized queue drains in
**parent-chain order from the last-sent hash regardless of arrival order**
(out-of-order arrivals park in the queue; only the service→write-thread channel
requires strict order, and the state's own drain guarantees it) — so fetches
and rayon conversions completing out of order are safe by construction. The
engine issuing futures in height order plus `spawn_fifo`'s FIFO ordering keeps
completions nearly in order in practice, so the unbounded state queue stays
~empty — a memory optimization, not a correctness requirement.

Two deliberate properties:

- **Slot retention until resolution**: the engine keeps the fetched block's
  `Arc` in its `Slot::Fetched` until the commit future resolves. This is free
  (the state queue holds the same `Arc`) and is what lets §4.6 resubmit
  reset-dropped descendants from memory instead of refetching them.
- **No depth-dependent timeouts**: verify-and-commit futures have no analog of
  the 480 s `BLOCK_VERIFY_TIMEOUT` trap; liveness is monitored by frontier
  progress, not per-future timers. Corrupt bodies are detected at the verify
  stage — up to 1,024 blocks ahead of the disk frontier — so the refetch budget
  is the whole pipeline's drain time, usually costing zero frontier stall.

Dedup: one slot per height in the ring is the single dedup point; hedge losers
are cancelled via the slot's `AbortHandle`.

Genesis is simply `list[0]` fetched by hash — no `request_genesis` special case in
the engine. (The legacy syncer keeps its genesis machinery for the flag-off path;
regtest commits its genesis directly to the state.)

### 4.5 Disk overflow tier (`cache.rs`) — download each block at most once

**Invariant**: a block copy is never downloaded again once **(1)** its header
hash matches the pinned known-hash list entry for its height **and (2)** its
body has been confirmed to belong to that header. Condition (1) is established
at receipt (the connection `Handler` hash check) and re-pinned by `convert`.
Condition (2) is the merkle-root check in `convert` — run exactly once per
block: inside the commit call for memory-tier blocks, at promotion for
disk-tier blocks. It fully confirms the
body for v1–v4 transactions (txids commit to the full serialized transaction);
for v5 transactions the merkle root confirms the effecting data, and the
authorizing data is only confirmable at commit time (`hashBlockCommitments`
needs the history tree). Consequences:

- A copy that fails `convert` never met the condition — refetching it is
  *inside* the invariant, not an exception.
- A v5 copy that passes merkle but fails its auth-data commitment at commit
  never met condition (2) — the implicated cached copy is discarded and the
  block is refetched from a different peer, also inside the invariant.
- A copy that met the condition is sacred: it lands on disk rather than being
  dropped, and is promoted from disk rather than refetched. Remaining
  exceptions are only (a) disk corruption of a cached entry (caught by
  load-time re-verification, refetched) and (b) process-crash loss of
  in-memory copies (bounded by the memory budget; see the write-ahead option
  below).

**The cache is the window's second tier, not an exception path.** The engine
generally keeps downloading **past the memory lookahead limit**, saving those
blocks to disk instead of holding them in memory while the state service works
through the commit pipeline:

- **Tier 1 (memory)**: `[base, mem_head)` — bounded by
  `known_hash_lookahead_bytes`; blocks here flow straight to verify-and-commit.
- **Tier 2 (disk)**: `[mem_head, disk_head)` — bounded by `IBD_SPAN_MAX` only
  (no byte cap and **no config option**: cached blocks are blocks the chain
  state is about to store anyway, so the transient double-storage is space the
  volume must have regardless; worst case ≤ span × max block ≈ 128 GiB,
  typically far less, continuously evicted); blocks are fetched and written to
  the cache **raw, with no verification beyond the connection `Handler`'s
  header-hash match**. As commits free memory budget, the refill step
  **promotes** the lowest cached heights through the normal verify-and-commit
  path (the async file read happens inside the promoted block's stage-2
  future; deserialization runs inside the `spawn_fifo` closure with `convert`
  — the engine loop never does I/O inline). **Each block is verified exactly once in the common case** — memory-tier
  blocks inside their commit call, disk-tier blocks at promotion. (Bounded
  exceptions, all re-verification of bytes already held, never re-download:
  commit-reset resubmission re-runs `convert` on the retained `Arc` (§4.6),
  and blocks flushed raw at shutdown are verified at post-restart promotion.)
  Verifying before the cache write would be pure double work (cached bytes are
  untrusted on load, so promotion must re-run `convert` regardless, and
  `CheckpointVerifiedBlock` is not persisted) — and the saved tx-hashing pass
  matters most in the spam-attack era, exactly where the disk tier is most
  active. The entry's **source peer addr is stored with it** (sidecar field in
  the filename or index) so a corrupt body discovered at promotion still gets
  attribution: exclusion + misbehavior report if the peer is still connected,
  and the refetch goes to a different peer either way.

This decouples network throughput from state-commit throughput in general: in
write-thread-bound eras the network races ahead onto disk and only goes idle
when the disk budget fills; transient slow patches (peer churn, gap stalls)
no longer drain the pipeline. Honestly stated: the disk tier smooths variance
and keeps the network saturated — steady-state throughput is still the
slowest stage's rate, and the tier adds one sequential write+read per
overflow block on the state volume, so the A2 harness must measure its
interaction with RocksDB write load.

Mechanism — a byte-bounded block store under the state cache directory
(`<cache_dir>/ibd-block-cache/`, one file per block, `<height>-<hash>.bin`,
canonical `zcash_serialize` bytes; no per-block fsync — torn writes are caught by
load-time verification):

- **Other writers besides overflow fetch-ahead**: (1) the commit-reset path
  (§4.6) — descendants dropped by the write thread are already retained as
  `Slot::Fetched` Arcs and may be flushed to the cache instead of held; (2)
  graceful shutdown — the engine flushes `Slot::Fetched` blocks raw (no
  verification on the way out — fast exit; promotion verifies after restart)
  so a restart resumes without refetching; in-flight stage-1 fetches are
  aborted, not flushed. Supervisor-driven engine restarts (§4.7) do the same
  flush before rebuilding the window.
- **Slot integration**: cached heights are `Cached { bytes }` — never
  re-requested, excluded from the memory byte budget, counted against the disk
  budget.
- **Restart**: on engine (re)start, scan the cache dir, prune entries outside
  `(state_tip, list.max_height()]`, and mark surviving heights `Cached`. Cached bytes are **untrusted
  input**: on load they go through the full `convert` path (hash vs
  `list[height]`, merkle, linkage) exactly like a network response.
- **Eviction**: an entry is deleted once its block commits (lazily batched);
  fetch-ahead pauses only at the `IBD_SPAN_MAX` ring bound. The directory is
  removed entirely at `Completed` handoff.

Optional hardening (a planned `known_hash_cache_write_ahead` config, off by
default, added together with its implementation): also
spool **memory-tier** blocks to the cache raw on arrival (sequential append,
deleted on commit), closing exception (b) at the cost of writing the full
chain (~100 GB) to disk once more across IBD. Off by default
because crash-restart is rare and the loss is bounded; the harness can
quantify the overhead if we want it on. (Disk-tier blocks are on disk by
construction and never affected by crashes.)

### 4.6 Commit-failure handling

On a commit error (write-thread reset path): re-derive the frontier from
`latest_chain_tip`. Descendants dropped by the write thread are **resubmitted
from the `Arc`s retained in their `Slot::Fetched` entries** (§4.4) — or re-fed
from the disk tier if flushed there — never refetched. Resubmission re-runs
`convert` on the retained `Arc` (a `CheckpointVerifiedBlock` is never stored in
a slot): a re-*verification*, not a re-download — gate 8 counts downloads. Only the failed
height itself is refetched, **from a different peer**, with the previous
source excluded and a misbehavior report (the source peer addr is carried
through conversion for attribution — this matters for NU5+ auth-data
substitution, which is only detectable at commit; the retained/cached copy of
the failed block is discarded since it is the implicated copy). After
`COMMIT_FAILURE_ATTEMPT_LIMIT = 3` failures of a byte-identical block, declare
deterministic failure (list-vs-chain inconsistency or local DB corruption), log
a fatal diagnostic, stop the engine with an explicit error — never silent
fallback, never infinite redownload. `WriteTaskExited` / closed channels are
fatal-shutdown: the engine task returns `Err` and start.rs's task supervision
tears the process down.

### 4.7 Supervisor, restart, handoff (`ibd.rs`)

```rust
pub enum IbdOutcome {
    Completed { final_height: Height },  // committed through list.max_height()
    Declined(DeclineReason),             // NoList | AssetMissing | AlreadyPast | DisabledByConfig
    Degraded(DegradeReason),             // only above the mandatory floor (§4.1)
}
```

`run()` is a plain re-enterable function (fixes the one-shot defect of the
ibd-minimal prototype): every (re)start re-derives `next_commit` from
`best_tip_height() + 1`, restores blocks from the disk tier (§4.5), and
rebuilds the rest of the window empty; bounded loss ≤ memory-tier bytes.
The supervisor restarts after `IBD_RESTART_DELAY` on fatal-but-retryable
errors; the consecutive-failure counter resets whenever a restart makes progress.

Wiring (start.rs): `ChainSync::new` is still constructed at startup (its
`SyncStatus`/`RecentSyncLengths` handles feed mempool, gossip, progress, and the
health endpoints), but `sync()` is driven only after the engine returns. With no
`RecentSyncLengths` pushes during the engine phase, `is_close_to_tip()` is `false`
— the correct "mempool inactive, not ready" semantic, with no shim. The engine's
`JoinHandle` is registered in start.rs's task-supervision `select!` so ctrl-c and
sibling-task failures abort it cleanly; the engine drop closes the state channel
naturally.

After `Completed`/`Declined`/`Degraded`, the legacy syncer starts from a block
locator at the real tip — no special seam. The state's finalized→non-finalized
flip triggers on the first semantically-verified child of the last committed
block, exactly as today (verified: the trigger keys on the last-sent
checkpoint-verified hash, not on `max_checkpoint_height`, and self-heals on
transient races).

## 5. zebra-network changes

Re-implemented fresh on main (the syncer-performance originals predate 8 months of
`set.rs` drift), one concern per commit:

1. **Live peer height + height-aware routing**: `LoadTrackedClient` gains a
   monotonic live height raised by delivered blocks
   (`remote_height() = max(handshake, live)`); multi-hash `BlocksByHash` routes
   via tall-peer-filtered P2C. Mostly inert during the deep-history engine phase;
   directly improves the legacy tail.
2. **Stalled-tip eviction**: when the chain tip stalls for 10 minutes, evict one
   random peer and signal `MorePeers` (rate-limited to one per window). Protects
   the tail; inert during the engine phase (tip grows continuously).
3. **Per-peer sync diagnostics**: `blocks_received` counters + rate-limited peer
   set status logs (ready/unready counts, height distribution, per-peer delivered
   blocks and EWMA load). This is the engine's per-peer health signal and the
   operator's visibility into IBD.
4. **`PeerSetStatus` watch** (ready-peer count, median remote height) consumed by
   the scheduler to size `IBD_MAX_CONCURRENT_BATCHES`.

Explicitly **not** changed: inventory-registry semantics (the #10679 fix is
consumed, never modified); the connection `Handler` state machine. (A separable
follow-up may return accumulated blocks on timeout instead of discarding them —
safe for #10679 because absence ≠ `Missing` — but the engine does not depend on
it.)

## 6. Asset strategy (the ~103 MB list + size hints)

### 6.1 The spec constant

Per network, a compile-time constant pins everything a consumer needs **without
loading any chunk**:

```rust
pub struct KnownHashListSpec {
    /// The highest height covered by the list (e.g. 3_358_431 on mainnet today).
    pub max_height: Height,
    /// SHA-256 of each hash chunk file, in chunk order.
    pub chunk_hashes: &'static [[u8; 32]],
    /// SHA-256 of the per-block size-hint array (§6.2).
    pub size_hint_hash: [u8; 32],
}
```

`max_height` being a reviewed constant (not derived from file sizes) means the
§7.2 gate floor, state-init parameters, and `progress.rs` need no chunk I/O at
all, and a truncated/extended asset set is detected as a hard mismatch at load
(total bytes must equal `(max_height + 1) × 32` and chunk count must equal
`chunk_hashes.len()`).

### 6.2 Per-block size hints

Alongside the hash chunks, the list carries **1 byte per block**: the block's
serialized size as a fraction of the maximum block size, **rounded up** —
`hint = serialized_size.div_ceil(SIZE_HINT_UNIT)` with
`SIZE_HINT_UNIT = MAX_BLOCK_BYTES.div_ceil(255)` (= 7,844 B), so
`hint × SIZE_HINT_UNIT` is always an upper bound. Uses:

- **`RequestWeight` for the batched fetch layer** (§4.2): batches pack to the
  800 KB flush threshold with a priori per-block sizes — no adaptive size
  tracker, no estimate drift; rounding up keeps every pre-crossing prefix
  under the serving limit (see §4.2's overflow analysis).
- **Exact-in-advance byte accounting** (§3.3): in-flight bytes are bounded by
  hint upper bounds rather than EWMA estimates, eliminating the estimate-
  overshoot subtlety.

The array is ~3.36 MB for mainnet (one file, SHA-256-pinned by
`size_hint_hash`; load validation: length must equal `max_height + 1` and
every hint must be in `1..=255`), kept fully resident during IBD and dropped
at handoff. A
wrong hint is liveness-only (mis-packed batch → silent truncation → handled as
transport loss), never a correctness issue. Generation requires block sizes:
the `zebra-utils` tool sweeps a synced local node via `getblock` (explorers
were evaluated and rejected for bulk collection — page-scraping 3.4M blocks
takes weeks at polite rates; a local RPC sweep takes under an hour).

### 6.3 Distribution and integrity

The ibd-minimal loader resolves chunks via `env!("CARGO_MANIFEST_DIR")` — a
compile-time path that breaks every installed binary. Replacement:

- The `KnownHashListSpec` constants stay **in source** (the trust root never
  leaves review). Chunks live in `zebra-chain/src/parameters/known_hashes/` in
  git, but are **excluded from the published crate** (`Cargo.toml` `exclude`;
  crates.io would reject a ~103 MB package).
- Runtime resolution order: (1) `[sync] known_hash_list_dir` config override;
  (2) executable-adjacent (`./known-hashes/`, `../share/zebrad/known-hashes/` —
  release tarballs and docker images ship chunks here; chunks 00–21 are
  append-only so docker layers dedupe across releases); (3) platform data dir
  (`~/.local/share/zebrad/known-hashes`); (4) **verified self-download** from
  pinned HTTPS URLs (GitHub release artifacts), each chunk verified against its
  baked-in SHA-256 in memory before write and re-verified on load
  (config-disableable; privacy note in CHANGELOG).
- A tampered chunk is a hard error naming the chunk. While the checkpoint
  verifier still exists, a missing asset `Declines` to the legacy path; after
  deletion it is a hard, actionable error on mainnet/testnet.

### 6.4 Memory residency: windowed chunk loading

Disk reads are cheap — re-reading 100 MB even several times is fine; *retaining*
it is the waste. Residency model:

- **At startup**: read every chunk once, verify all SHA-256s against the spec
  constant and the total length against `max_height` — then **drop the bytes**
  (fail fast on tampering without holding 103 MB).
- **During the engine phase**: hold exactly the chunk containing the window,
  loaded as a flat `[[u8; 32]]` (4.8 MB, O(1) index), re-verified against its
  constant on each load; prefetch the next chunk when the window head crosses
  within `IBD_SPAN_MAX` of the boundary and drop the previous chunk when `base`
  passes it — so 1 chunk resident almost always, 2 near boundaries (~5–10 MB),
  plus the ~3.4 MB size-hint array. `IBD_SPAN_MAX` (65,536) < chunk span
  (150,000) guarantees two chunks always cover the window.
- **After the chain tip passes `max_height`**: drop all hash and size-hint
  memory entirely. The §7.2 gate and `progress.rs` need only the `max_height`
  constant; the restart spot-check loads one chunk transiently.

The spaced `checkpoint_list()` BTreeMap stays for the legacy path and tooling —
which also keeps the router's background walk on ~10k entries until deletion
removes it entirely.

## 7. Consensus surgery (final wave)

### 7.1 Deletions

| Area | What dies | LOC |
|---|---|---|
| `zebra-consensus/src/checkpoint.rs` + `checkpoint/types.rs` + tests | entire CheckpointVerifier | ~2,100 |
| `router.rs` | checkpoint field/arms, `init_checkpoint_list`, background full-list state walk | ~210 |
| `router/tests.rs` | checkpoint-routing cases | ~150 |
| `zebrad` sync.rs / downloads.rs | `max_checkpoint_height` threading, checkpoint lookahead branches, checkpoint concurrency constants | ~130 |
| exports, config, test helpers | `VerifyCheckpointError`, `MAX_CHECKPOINT_*`; `checkpoint_sync` config absorbed into a back-compat `InnerConfig` shim | ~60 |

`CheckpointList` itself stays in zebra-chain (testnet construction validation,
`zebra-checkpoints` tooling, progress ETA). `CheckpointVerifiedBlock` and
`CommitCheckpointVerifiedBlock` stay — they are the engine's commit path.

### 7.2 The commit gate (required, lands with the deletion)

Today the router arm `height <= max_checkpoint_height → CheckpointVerifier`
structurally prevents a semantically-verified block from reaching the
non-finalized queue mid-IBD. Without it, an adversarial gossiped or RPC-submitted
block at engine-tip+1 would pass the inbound height window, semantic-verify
(its parent's UTXOs are finalized), and **flip the state to non-finalized**,
permanently demoting the rest of IBD. The router's replacement (a thin semantic
gate with the same `Request`/Buffer surface, so inbound/RPC/sync callers are
untouched) therefore rejects `Commit` requests at or below a floor with a benign
`VerifyBlockError::BelowKnownHashRange { height, floor }`:

- While the engine is active: floor = engine final height (published via
  `watch<Option<Height>>`; set to `None` whenever the engine returns —
  `Completed`, `Declined`, **or `Degraded`** — after which only the permanent
  `mandatory_checkpoint_height` floor applies, so a post-degradation legacy
  syncer can semantically verify and commit from the real tip).
- Permanently: floor = `mandatory_checkpoint_height` (the code-level expression
  of "Zebra cannot fully validate pre-Canopy blocks"). For configured/custom
  testnets without a list: floor = `min(mandatory_checkpoint_height, available
  hash-source coverage)` with a documented reduced-trust warning. Regtest: 0.

Gossip's downloader already treats verifier errors as non-fatal drops; RPC
`submit_block` maps the error to the standard zcashd-compatible rejection.

The background list-walk is replaced by an O(1) spot-check (genesis +
`BestChainBlockHash(finalized_tip)` vs `list[tip]`), preserving
wrong-chain-on-restart detection without the walk.

**As-built update (2026-06-11).** The floor watch was dropped in favor of
constants. The gate floor is `mandatory_checkpoint_height` while the engine is
disabled, and is raised to the network's known-hash list max height while the
engine is enabled (`router::init` takes `known_hash_sync`): the list-max floor
exists only to stop a semantic commit from flipping the state mid-engine-sync,
so a legacy-path node with the engine off is gated only at the permanent
mandatory height. A `RouterError::BelowKnownHashRange` reaching the legacy
syncer is now surfaced as an actionable error (the range is engine-only;
re-downloading cannot help) instead of a silent retry loop.

**Router removal — done.** `BlockVerifierRouter` and `RouterError` are gone;
the floor check lives in `SemanticBlockVerifier::call` (with
`VerifyBlockError::BelowKnownHashRange`), and `router::init` buffers the
semantic verifier directly. The buffered error type is now `VerifyBlockError`
across the inbound, sync-download, and RPC `submit_block` paths.

**Open / planned follow-ups (need maintainer decision):**
- *RPC `submit_block` duplicate reporting.* An RPC submission of an
  already-committed below-floor block now reports `Rejected` (the gate fires
  before duplicate detection) rather than zcashd's `duplicate`. Restoring
  duplicate reporting for below-floor blocks (e.g. a state check before the
  gate) is a small, separable compatibility fix.
- *Degraded handoff below list max.* When the engine degrades between the
  mandatory height and the list max, the legacy syncer it hands off to still
  cannot commit that range. The handoff is a false promise there; the choice
  (keep restarting the engine forever with alarms, vs. exit fatally) is an
  operational-behavior decision left to the maintainer.
- *Flag-off fresh mainnet sync* below Canopy is unsupported without the engine
  (no checkpoint verifier); whether to hard-error at startup is open.

### 7.3 State-side renames (same commit)

`zebra_state::init`'s `max_checkpoint_height` → `max_finalizable_height` (it gates
the finalized write path and backup-restore); the near-final-checkpoint UTXO
window keeps its semantics with a margin of 4,096 (≥ commit pipeline + reorder
in-flight bound), replacing today's `checkpoint_verify_concurrency_limit * 3`.
The `FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT` workaround (#5125) is retained, keyed
on the final known height. Progress bars (`progress-bar` feature) lose their
checkpoint-verifier bars; the engine registers equivalents
(fetched/converted/committed).

### 7.4 Testnet

The spaced testnet list cannot drive known-hash sync (intermediate hashes are
unknown). Strategy: **assemble the testnet every-block list at E2 time — the
last step — anchored against the existing spaced checkpoints** (a pure asset
addition to the network-generic loader; ~3.1M blocks ≈ ~100 MB, self-download by
default rather than bundled). List assembly is deliberately deferred: a local
testnet sync is already running in the background and will likely be ready
before E2 needs it.

1. A small tool in `zebra-utils` assembles every testnet block hash and block
   size 0..tip from the synced local node (`getblockhash`/`getblock` sweep —
   bulk collection from explorers is impractically slow; see §6.2).
2. **Anchor verification**: every height present in the reviewed, already-trusted
   spaced `test-checkpoints.txt` (10,144 entries, ≤400 apart) must match
   exactly — one mismatch fails the run. This roots the assembled data in the
   same trust anchor the node ships today.
3. Cross-checks: explorer spot-checks at sampled heights (a random sample plus
   all network-upgrade activation heights), since a full-range second sweep is
   impractical. Disagreement anywhere fails the run.
4. The tool emits the chunked `.bin` format, the per-block size-hint array
   (§6.2), and the `KnownHashListSpec` constants; the loader's genesis/alignment
   checks apply as on mainnet.

Defense in depth for intermediate (non-anchor) heights: the engine requests each
block by its pinned hash and checks `prev_hash == list[h-1]` linkage at convert
time, and every ≤400-block run terminates in a spaced-checkpoint-anchored hash —
a wrong intermediate explorer hash can only cause NotFound stalls (liveness),
never acceptance of an off-chain block.

The deletion commit is **gated on that list landing** and on a testnet A/B sync
test. If assembly slips, the checkpoint verifier may survive exactly one extra
release (time-boxed), serving testnet only — mainnet stops routing through it
when the engine flag defaults on.

## 8. Observability

- Metrics (per CLAUDE.md prefixes): `ibd.fetched.block.count`,
  `ibd.converted.block.count`, `ibd.committed.height`, `ibd.window.bytes`,
  `ibd.inflight.batches`, `ibd.gap.hedge.count`, `ibd.stall.seconds`,
  `ibd.peer.notfound.count{addr}`, `ibd.lookahead.blocks` (the emergent
  auto-scaled lookahead — directly shows the per-era self-tuning),
  `ibd.cache.bytes`, `ibd.cache.written.count`, `ibd.cache.promoted.count`,
  `ibd.cache.restored.count`, and `ibd.duplicate.download.count` (the §4.5
  invariant made observable — increments only on the documented exceptions, with
  a `reason` label), plus a compatibility increment of
  `sync.downloaded.block.count` so existing dashboards keep working.
- The `state.checkpoint.*` and `zcash.chain.verified.block.*` families are
  emitted by the state itself and survive unchanged (this also keeps the
  `create_cached_database` acceptance regex alive).
- Progress: `progress.rs` keeps working unmodified (reads `latest_chain_tip` +
  `sync_status`); `getblockchaininfo` `verificationprogress` is tip-derived and
  unaffected. The engine adds a one-line frontier progress log
  (`ibd progress: committed=…, frontier=…, window=…, peers=…`) on the existing
  60 s cadence.

## 9. Phases and commits

Each commit is buildable and testable on its own. Nothing changes default
behavior before Phase D. Reviewers: check each commit against the listed spec
sections; the maintainer verifies runtime behavior per the listed checks.

### Phase A — Spec and measurement baseline

| # | Commit | Contents | Spec | Maintainer verification |
|---|---|---|---|---|
| A1 | `docs(design): add known-hash IBD engine design` | this document | — | — |
| A2 | `test(sync): add per-era IBD performance benchmark harness` | cherry-pick `copy-state-perf:60171315f` (`zebrad/tests/common/sync_perf.rs`, ~457 LOC + acceptance hooks), adapted to main. **Reference only — do not run**: its per-sample seeding replays nearly a full IBD through `copy-state` for high sample heights; it is completely rebuilt at F4 before any benchmark runs | §1, §12 | deferred to F4 (the rebuilt harness produces the baselines) |

### Phase B — Known-hash list infrastructure (zebra-chain)

| # | Commit | Contents | Spec | Maintainer verification |
|---|---|---|---|---|
| B1 | `feat(chain): add KnownHashList with verified windowed chunk loader` | `known_hashes.rs`: `KnownHashListSpec` constant (max_height + chunk hashes + size-hint hash, §6.1), windowed loader with layered path resolution (§6.3–6.4: verify-all-at-startup-then-drop, 1–2 resident chunks, drop-at-handoff); 23 mainnet `.bin` chunks lifted file-wise from `ibd-minimal` (**never** the commit — it contains a 15.5 GB blob; add `zebra_state.tgz*` to `.gitignore`); mainnet size-hint array generated from the maintainer's synced node (§6.2); adapted 17-test suite + path-resolution + residency tests; `Cargo.toml` exclude | §2, §6 | `cargo test -p zebra-chain known_hashes`; verify chunk SHA constants + size hints against an independently generated list |

### Phase C — zebra-network peer health and routing

| # | Commit | Contents | Spec | Maintainer verification |
|---|---|---|---|---|
| C1 | `feat(network): live peer height tracking and height-aware block routing` | fresh re-implementation (reference: syncer-performance `bb27e8603`/`3b50c3533`/`e78813fde`) | §5 item 1 | set vectors tests |
| C2 | `feat(network): rate-limited peer eviction on stalled chain tip` | reference `effde63d0` | §5 item 2 | set vectors tests; soak on legacy path |
| C3 | `feat(network): per-peer block delivery counters and peer set diagnostics` | reference `peer-set-sync-logging:d0cf73290`; adds `PeerSetStatus` watch | §5 items 3–4 | log output inspection on a live sync |

### Phase D — The IBD engine (zebrad)

| # | Commit | Contents | Spec | Maintainer verification |
|---|---|---|---|---|
| D1 | `feat(zebrad): add IBD engine skeleton behind sync.known_hash_sync (default off)` | `components/ibd.rs` + `ibd/` layout, `IbdEngine`/`IbdOutcome`/config types, ring window allocation; engine spawns and returns `Declined` | §3, §10 | builds; flag-off behavior byte-identical |
| D2 | `feat(ibd): engine task with ring window and weighted batched fetch` | `engine.rs` + `fetch.rs` + `peer_stats.rs`: per-block staged futures (§4.1), `FetchBatcher` under tower-batch-control with size-hint `RequestWeight`; `engine_prop.rs` property tests (issue-once-until-committed, byte+count bounds under random completion order, NotFound vs transport classification, batch flush thresholds incl. a crafted crossing batch served in full) | §4.1–4.2 | property tests; mocked-peer vectors |
| D3 | `feat(ibd): verify-and-commit tower service over the state with rayon merkle checks` | `convert.rs`: pure `convert()` + `VerifyAndCommit` tower service using `zebra_consensus::primitives::spawn_fifo`; vectors for bad-merkle / duplicate-tx / wrong-height / broken-link against a `MockService` state | §4.3 | vectors incl. header-valid/body-swapped block; parallelism smoke test (N conversions overlap on rayon) |
| D4 | `feat(ibd): commit pipeline with gap-priority hedging and reset recovery` | stage-2 continuation caps + `Slot::Fetched` retention + hedge/abort plumbing in `engine.rs`; `engine_prop.rs` additions (random completion order in → state commits parked safely; exact byte accounting; commit-reset resubmits from retained Arcs); end-to-end `ibd/tests/vectors.rs` over `MockService` peers + state | §4.4, §4.6 | commit-order fuzz; mid-stream NotFound storm test; injected commit-reset resubmission test |
| D5 | `feat(ibd): disk overflow tier so each block is downloaded at most once` | `cache.rs`; `Tier::Disk` raw fetch-ahead past the memory budget (verify once, at promotion; source-addr stored for late attribution), promotion/eviction, `Slot::Cached`, restart scan; load-time reverification vectors (corrupt entry, truncated file, stale entry below tip) | §4.5 | kill/restart resumes from the disk tier with `ibd.duplicate.download.count == 0`; corrupt-entry refetch test; harness A/B of overflow I/O vs RocksDB write load |
| D6 | `feat(zebrad): run IBD engine before legacy syncer when enabled` | start.rs wiring (engine task in the supervision `select!`, state-init max height sourced from the list when flag-on, `ChainSync` constructed at startup / driven on handoff), metrics, progress log | §4.7, §8 | flag-on `sync_until(5_000)` acceptance; flag-off + testnet + regtest identical to main; ctrl-c mid-IBD clean shutdown (cache flushed) |

### Phase E — Default-on and consensus removal

| # | Commit | Contents | Spec | Maintainer verification |
|---|---|---|---|---|
| E1 | `feat(zebrad)!: enable known-hash sync by default on mainnet` | one-line default flip + CHANGELOG | §11 gates 1–8 | full gate suite (§11) |
| E2 | `feat(chain): add testnet every-block known-hash list` | deferred to last (local testnet sync running in the background); list + size-hint array assembled from the synced node and/or a trusted explorer, anchored against every entry of the existing spaced `test-checkpoints.txt` (§7.4); assembly/verification tool in `zebra-utils`; assets + `KnownHashListSpec` constants | §6, §7.4 | dual-source cross-check (node vs explorer); spaced-anchor verification log |
| E3 | `refactor(consensus)!: remove checkpoint verifier and gate commits below the known-hash floor` | deletion inventory (§7.1), semantic gate + `BelowKnownHashRange` (§7.2), O(1) spot-check, state renames (§7.3), config shims, progress bars, `submit_block` mapping, crate CHANGELOGs | §7 | mid-IBD gossip/RPC injection test (finalized channel stays open); testnet A/B sync; v5.1.0 config-compat test |

### Phase F (final) — State write performance

Per the maintainer's direction these land last; they are independent of the
engine and lowest-impact to its review.

| # | Commit | Contents | Spec | Maintainer verification |
|---|---|---|---|---|
| F1 | `perf(state): tune RocksDB for write-heavy initial sync` | cherry-pick `copy-state-perf:475d6000e` (write buffers, background jobs; `disk_db.rs` only, no format change) | §12 | harness A/B per era |
| F2 | `perf(state): pause auto-compaction and WAL during known-hash IBD` | rework `924008ebb`: RAII drop-guard + re-enable-on-open (the original's straight-line re-enable is not exit-safe), L0 file-count guard with periodic manual compaction (compaction-off degrades the write thread's own point reads), trigger keyed on engine-active | §12 | kill -9 during paused compaction → restart healthy; harness A/B with toggle on |
| F3 | `perf(state): pipeline checkpoint commits across lookup/write stages` *(gated)* | adaptation of `state-writer-performance` three-thread pipeline (`commit_checkpoint_block`, `lookup_spent_utxos`, bounded crossbeam channels); **only if** the harness shows the write thread binding after F1–F2; needs its own correctness review (anchor-check semantics on the checkpoint path) | §12 | harness verdict first; then full gate re-run |
| F4 | `test(sync): rebuild the per-era benchmark harness with cached seed states` *(time permitting; replaces A2's internals before any benchmark runs)* | **cached seed states**: state dirs keyed by (network, sample start height, db format version), built once via slow `copy-state` and reused across runs and branches; **per-run writable snapshots** of a cached seed via RocksDB `Checkpoint` hard-links (new `disk_db` plumbing — none exists today) instead of replaying or copying ~100 GB; optional follow-up: radically faster `copy-state` that copies RocksDB files directly (`SstFileWriter` + `ingest_external_file`) as its own zebra-state utility | §12 | first cached-seed build per height; A/B reports across branches reuse seeds byte-identically |

## 10. Config surface

Additive fields on the existing `[sync]` section (serde `default` keeps old
configs parsing under `deny_unknown_fields`):

```rust
/// Enable known-hash initial sync. Defaults off until validated (Phase E1 flips it).
pub known_hash_sync: bool,                 // false → true at E1
/// Initial-sync lookahead limit, as a byte budget (replaces the legacy
/// block-count `checkpoint_verify_concurrency_limit`). The block-count
/// lookahead is auto-scaled from the bundled per-block size hints.
pub known_hash_lookahead_bytes: usize,     // 268_435_456 (256 MiB)
/// Seconds a frontier-critical block may be in flight before a hedged refetch.
pub known_hash_gap_hedge_secs: u64,        // 5
/// Override directory for the known-hash chunk files.
pub known_hash_list_dir: Option<PathBuf>,  // None → layered search (§6)
```

Planned fields, added together with their implementations:
`known_hash_list_download` (verified self-download of missing list chunks) and
`known_hash_cache_write_ahead` (spool every fetched block to the cache).

Deprecations (E3): `sync.checkpoint_verify_concurrency_limit` (+ `lookahead_limit`
alias) and `consensus.checkpoint_sync` become accepted-but-ignored via back-compat
shims (warn when set) — the block-count checkpoint lookahead has no new-engine
analog; its replacement is the `known_hash_lookahead_bytes` byte budget with the
block count auto-scaled from size hints. `download_concurrency_limit`,
`full_verify_concurrency_limit`, `parallel_cpu_threads` are unchanged (tail +
rayon still use them).

## 11. Validation gates (before E1)

1. **Tip-hash identity**: `getblockhash` equals a main-built node at heights
   {1, 10k, 100k, 419,200, 1,046,400, 1,687,104, 2,000,000, 3,000,000,
   3,358,431}; identical committed tip after steady state.
2. **Zero stalls**: one full genesis→list-max mainnet run with zero fatal engine
   restarts; 24 h soak with induced peer churn (drop 50% randomly) always
   recovers.
3. **Throughput**: ≥ 2× aggregate via full-run wall clock (interim signal,
   no seeding needed); per-era blocks/s ≥ main in every era via the rebuilt
   F4 harness once it lands.
4. **Memory**: every §3.3 bound holds (gauges/property tests per row);
   `ibd.window.bytes` never exceeds the budget; RSS ≤ main + 700 MB
   (256 MiB lookahead + worst-case in-flight batches + resident chunks +
   slack — see the corrected §3.3 row 1).
5. **Crash safety**: kill -9 at three random heights (one during paused
   compaction, post-F2) → resumes from tip+1, DB healthy, cached blocks restored
   without refetch.
6. **Testnet/regtest unaffected**: full legacy-path testnet sync matches main;
   regtest suite green; flag-off mainnet byte-identical.
7. **Config compat**: a v5.1.0 zebrad.toml parses and runs.
8. **No duplicate downloads**: `ibd.duplicate.download.count` stays 0 across the
   full run, the soak (gate 2), graceful restarts, and induced commit-reset
   tests — exceptions only with `reason` ∈ {corrupt-cache, crash-loss,
   unconfirmed-copy} and each occurrence explained (`unconfirmed-copy` = a copy
   that failed convert or its commit-time auth-data check, i.e. never met the
   §4.5 invariant condition — invariant-consistent, but still counted for
   observability).
9. *(E3 additionally)*: testnet list landed + gates 1–2 re-run on testnet;
   mid-IBD gossip/RPC injection cannot flip the state to non-finalized; a
   backup-restore cycle and a concurrent in-place format upgrade behave
   correctly with the new final height (~3.36M vs ~1M today).

## 12. Throughput model and the measurement mandate

Rough per-stage estimates (150 ms RTT, 4 MB/s per peer, 1 Gbps downlink, 8
cores): small-block eras bind on the **state write thread** (thousands of
blocks/s); the medium era binds on the **network** at 25 peers (~290 blocks/s)
and the write thread at 50+; the large/sandblasted era binds on **node downlink**
(~70–80 blocks/s on 1 Gbps). Convert never binds once parallel. These numbers
justify the phase ordering — the engine first (removes discovery + serial
verification), state pipeline last (gated on the harness showing the write
thread binding). All constants in §3.2 are re-tunable from harness data; no
performance claim in this document is final until the rebuilt harness (F4)
reproduces it. Until F4 lands, the interim throughput signal is full-run
wall clock from the gate 1–2 genesis→list-max runs (which need no seeding);
per-era A/B windows and the F-phase I/O measurements wait on F4.

## 13. Risk register (abbreviated)

| Risk | Sev | Mitigation |
|---|---|---|
| Gossip/RPC block at engine-tip+1 flips state to non-finalized mid-IBD | Critical | §7.2 gate; injection test (E3 gate 9) |
| Merkle/dup-txid check dropped in convert | Critical | reuse `merkle_root_validity` verbatim; body-swap vectors (D3) |
| v5 auth-data substitution → commit-reset amplification | High | peer attribution + exclusion + 3-failure deterministic detector (§4.6); implicated cache copy discarded |
| Scheduler recreates #10679 poisoning | High | engine never writes the registry; exclusion only on explicit NotFound; injected-timeout regression test |
| Reorder-buffer OOM in 2 MB-block era | High | bytes-first caps; frontier-only mode at cap |
| One-shot fallback regression (ibd-minimal defect) | High | re-enterable `run()`; stall ladder loops forever below mandatory floor |
| Installed-binary asset resolution breaks | High | layered loader + verified self-download; hard error, never silent |
| Testnet IBD broken by deletion | High | E3 gated on E2 + testnet A/B |
| Compaction/WAL toggle leaves DB degraded on crash | Med | RAII guard + re-enable-on-open + kill test (F2) |
| Oversized getdata batches → silent 20 s stalls | Med | 16-item cap + 800 KB flush threshold; serve-after-one semantics serve the crossing block (§4.2 analysis); regression test with a crafted crossing batch |
| Wrong/stale size hints mis-pack batches | Low | liveness-only (truncation → transport loss); hints pinned by `size_hint_hash`; regen with the list |
| 15.5 GB blob carried from ibd-minimal | Med | file-wise asset lift only; `.gitignore`; CI size check |
| Eclipse/total stall looks like success | Med | frontier watchdog + 60 s warns + not-ready health status |
| Cache poisoning / corrupt disk-tier entries accepted | High | cache is untrusted input: full `convert` re-verification on load (hash vs list, merkle, linkage); corrupt entries discarded + refetched |
| Disk tier peaks large when state lags network | Low | by design (state needs the space anyway); span-capped; continuously evicted; dir removed at handoff; harness measures I/O interaction |
| Assembled testnet list wrong between anchors | Med | §7.4: spaced-checkpoint anchoring + explorer spot-checks; wrong intermediates can only stall (NotFound), never be accepted |

## 14. Explicitly deferred

- Three-thread state pipeline (F3) until the harness shows the write thread
  binding; BlobDB and other format-affecting RocksDB changes.
- Checkpoint-sync disk I/O reduction, e.g. created-then-spent UTXO elision
  (§15.2) — very late phase, **separate PR**.
- Legacy-syncer tail tuning (FANOUT 3→2, restart-elimination loop) — orthogonal;
  the legacy syncer stays byte-identical through E2 for reviewability.
- Connection `Handler` partial-on-timeout change — separable network PR.
- `Request::Commit(Arc<Block>, Hash)` hash threading (18-file signature change
  saving one header hash per block — absorbed by parallel convert instead).
- Next-gen transport/state-chunk distribution, PoW-skip beyond the pinned list,
  assumeutxo-style snapshots.

## 15. Design evolution log and open questions

### 15.1 Resolved by maintainer direction (now in the main spec)

- **Single-task engine** (an open-question proposal in an earlier revision): adopted and refined — §4.1.
  Refinements over the original proposal: a fixed-capacity ring — per the
  maintainer's preference for an existing implementation over a hand-rolled
  `Box<[Slot]>`, this is **std's `VecDeque` with `with_capacity(IBD_SPAN_MAX)`**
  (a true ring buffer internally; preallocated once from config at startup,
  never grows while the span-cap invariant holds, guarded by a `debug_assert`).
  `arraydeque` (in-tree only transitively via `yaml-rust2`) was considered and
  rejected: const-generic capacity can't take the config-driven size, and its
  inline storage (~2.6 MB) would need boxing anyway. And the two `FuturesUnordered` collections
  **unified into one** — a single lifecycle future per block (fetch → verify →
  commit), with network batching moved into a tower-batch-control layer (§4.2)
  using the list's per-block size hints (§6.2) as `RequestWeight`. The
  weight-aware batch support already exists on main
  (`tower-batch-control/src/lib.rs:124`, `worker.rs:54,82`) — no crate changes
  needed. Residual checks during D2: the engine loop never awaits inline;
  `Slot::Fetched` retains the block `Arc` until commit resolution (load-bearing
  for §4.6 reset recovery); `Tier::Disk` issuance keeps overflow fetch-ahead
  on disk (§4.5). Per-block work is **staged futures** (fetch stage, then an
  engine-pushed verify+commit stage) — the stage boundary is what makes the
  commit-pipeline caps and slot retention implementable; a monolithic
  fetch→verify→commit future cannot be paused or observed mid-flight.
- **`Engine` is a future, not a `Stream`** (decided after maintainer query):
  its natural contract is `async fn run() -> IbdOutcome` — it owns scheduling
  and has no per-item consumer, unlike the legacy `Downloads: Stream` whose
  shape encodes the old syncer/downloader split. A hand-written `poll_next`
  over multiple sources + timers is also the known waker-bug class (the
  syncer-performance dual-queue `poll_next` deadlock risk); the `async fn` +
  `select!` shape makes the push-without-repoll footgun structurally
  impossible. `FuturesUnordered` remains the internal stream; progress is
  exposed via `watch` channels.
- **Windowed chunk residency** (an earlier revision's "streaming chunk loading" open question): adopted
  as the primary loader design — §6.4. Startup verifies all chunks then drops
  them; 1–2 chunks resident during IBD; everything dropped once the tip passes
  `max_height`.
- **`KnownHashListSpec` constant** (max_height + chunk hashes + size-hint hash)
  — §6.1; **per-block size hints** — §6.2.
- **Disk overflow tier** (maintainer-directed): the cache generalized from an
  exception-path spill (stall/pressure triggers) into the window's second
  tier — the engine generally keeps downloading past the memory lookahead and
  saves those blocks to disk while the state works through the commit
  pipeline (§4.5). No byte cap and no config option: cached blocks are blocks
  the chain state is about to store anyway, so the transient double-storage is
  space the volume needs regardless; bounded by the ring span. Disk-tier
  blocks are written **raw** and verified exactly once, at promotion —
  verify-before-write was dropped as double work (cached bytes are untrusted
  on load regardless), saving a tx-hashing pass exactly where it is most
  expensive (the spam era); the stored source addr preserves attribution.
- **Byte-budget lookahead** (maintainer-directed): the config exposes only
  `known_hash_lookahead_bytes`; the block-count lookahead auto-scales from the
  size hints within that budget (§4.1), replacing the legacy block-count
  `checkpoint_verify_concurrency_limit` semantics entirely.

### 15.2 Checkpoint-sync disk I/O reduction (very late phase, separate PR)

During the known-hash phase the write thread persists UTXOs that a later block
in the same sync window provably spends — a create-then-delete pair in RocksDB
that compaction must later clean up. The spam-attack era is the worst case:
enormous volumes of dust outputs created and spent within short spans.

**Key enabler — zero-hash future-spend harvesting.** The disk overflow tier
(§4.5) holds many blocks ahead of the commit frontier, and a transaction's
inputs name the txids they spend *explicitly* — so the set of outpoints spent
in the next H blocks can be harvested by mere iteration over the
already-deserialized blocks, with **no transaction hashing** (the expensive
part of `convert`, and the dominant CPU cost in the spam era). On the other
side of the query, the committing block's own txids are already computed for
its state indexes — so "is this freshly-created output spent within the
horizon?" is a free set-membership test on both ends. During the known-hash
phase this knowledge is *certain*, not speculative: the list pins the chain,
so a harvested future spend will definitely commit. Memory bound: a
bloom/cuckoo filter over the horizon's outpoints (~10 bits/entry; tens of MB
for spam-era spans) — a false positive merely skips an elision, never breaks
correctness.

Candidate uses, in ascending risk order:

1. **Spent-UTXO read-ahead**: prefetch/warm the UTXO records the write thread
   will need (its serial RocksDB point reads are a known cost) — no
   consensus-path change at all.
2. **Within-batch elision**: accumulate multiple blocks into one atomic
   RocksDB write batch and elide create+spend pairs that fall entirely inside
   it — crash-safe by construction (the pair never reaches disk), and needs no
   look-ahead beyond the batch itself.
3. **Cross-batch elision** (deferring an output's write because a harvested
   spend lands a few batches later): the largest win in the spam era, but it
   needs explicit crash-consistency — a crash between the elided create and
   the spend leaves the spender unable to resolve the outpoint on restart —
   so it requires a persisted deferral record or recovery-time re-derivation.

Caveats recorded now: address-index entries (balances, tx-by-address history)
must still be written — only the UTXO column-family pair elides; interaction
with the F2 compaction toggle and the F3 pipeline must be measured, not
assumed; and any change here is behavior-sensitive on the finalized write
path, so it ships as its own PR with its own review, after F3's verdict.
