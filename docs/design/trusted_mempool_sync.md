# TrustedMempoolSync — design

Status: **draft / planning** (local; not for upstream until issue
[#10761](https://github.com/ZcashFoundation/zebra/issues/10761) has Zebra-team
acknowledgment). Tracking issue: #10761.

A `TrustedMempoolSync` lets a separate, fully-trusting process keep a live,
in-process replica of a primary Zebra node's mempool — content **and**
transaction-lifecycle observability — driven by the indexer gRPC instead of
polling JSON-RPC. It is the mempool analogue of
[`TrustedChainSync`](../../zebra-rpc/src/sync.rs), built for Zaino.

A first attempt (`zebra-rpc/src/mempool_sync.rs`, private fork PR #6) was
discarded. This document is the redo, with a deliberately thick planning phase.

## 1. Why this is *not* the same problem as `TrustedChainSync`

`TrustedChainSync` replicates an append-only chain that is partly in memory
(non-finalized) and partly on disk (finalized), with a secondary DB doing the
heavy lifting. The mempool is the opposite:

- **All in memory, small, fast to access.** No DB, no catch-up, no
  finalized/non-finalized split.
- **High churn, short-lived.** A transaction typically lives 75 s – ~5 min
  (mined within a block or a few). The set turns over continuously.
- **Fundamentally a stream of events, not a static set.** What Zaino cares about
  is *where each transaction is in its lifecycle, and why it moved* — including
  transactions that never enter the verified set (queued, failed verification,
  evicted before verification, …).

This is a well-studied general problem (replicate a small in-memory keyed
collection to remote followers over a change stream). The state of the art is
the Kubernetes-style **list + watch + reconcile** informer, backed by a
monotonic version cursor (etcd's `revision`). See §7 for the research and how we
deliberately use a *cheaper* subset of it.

## 2. Requirements (locked)

| Axis | Decision |
| --- | --- |
| Fidelity | Transaction content by id **and** lifecycle observability (stage + reason), including specific verification errors. |
| Consumer | Zaino needs **both** a queryable mempool *and* the observation feed, equally. |
| Consistency | **Very strong** for set membership: never expose a tx the source dropped, never drop a tx the source kept. |
| Latency | Millisecond freshness. |
| Protocol | Free to redesign the indexer gRPC proto. |
| Topology | Separate process (not in-process), one source → one-or-more followers; keep the simple case simple. Fan-out must not complicate the design. |
| Deterministic replay | **Dropped.** Data is small; re-snapshot on any gap instead of replaying a log. |
| Size | Multi-commit series, may exceed 2k LOC total, but **each commit individually reviewable in a day or two**. |

## 3. The core insight: two layers with two consistency models

The mempool decomposes into two things that need *different* guarantees, and
conflating them was the central design error.

### 3a. Derivable state — strong, incremental + checksum-verified

The **queued set** (transactions in the download/verify pipeline) and the
**verified set** (the actual mempool, with fee/sigop/ZIP-317 metadata) are
*recoverable*. The replica is built **purely by applying lifecycle operations
incrementally** as they are notified — there is no fat snapshot message in
steady state.

The strong guarantee is upheld by **anti-entropy**, not by the event stream
alone: an event stream is at-least-once, so a single missed/duplicated/reordered
`Removed` could leave a phantom transaction in the replica forever. So every
batch carries an inline **checksum** computed by the source over its full replica
projection *after* that batch settles (see §3a-1). The follower applies the
batch's operations atomically, recomputes the checksum over its own state, and
compares:

- **match** → converged; cheap (a hash, not a state transfer). This is the
  common case and the only steady-state verification cost.
- **mismatch** → divergence detected → **repair** (re-bootstrap, §5).

This actively detects silent drift on a *live* stream (plain re-snapshot-on-gap
only heals on disconnect). Carrying the checksum *in* each batch bounds undetected
divergence to a single batch — the tightest bound available, and cheap because the
set is small — which is the honest form of "very strong" over an RPC boundary.

**Invariant §3a-1 — what the checksum covers.** The checksum covers the full
**replica projection**: the verified set *and* the queued set (`{txid → stage}`),
i.e. exactly the retained state the follower can reconstruct from the event
stream. It does **not** cover (a) source-internal machinery the replica doesn't
model (download timers, retry counts, in-flight verification handles), nor (b)
the transient observation layer (§3b), which isn't retained state. Because the
checksum is computed by the source over its *post-batch* state and shipped *in*
that batch, the follower compares at the exact same logical point (after batch
N) — so the queued set, despite being high-churn, never false-mismatches from
propagation lag the way it would under out-of-band periodic checkpoints. The
governing rule: *every mutation to checksummed state is an event in the batch*,
which holds by construction. The checksum is a plain hash of everything in the
projection — cheap enough on both sides to compute on every batch.

**Bootstrap is still incremental.** A checksum cannot reconstruct transaction
*content*, so on connect (and on repair) the source replays its current state as
ordinary `Added`/`Queued` events, terminated by an `InitialSyncComplete`
bookmark (the k8s `SendInitialEvents` WatchList model). There is no separate
snapshot type — just bootstrap batch(es) of the same events, then a live
`MempoolBatch` per change cycle, each carrying its own post-batch checksum. The
content transfer happens exactly once per bootstrap; the checksum optimizes
everything after.

### 3b. Transient observations — best-effort, gap-marked

The **reasons** a transaction moves (`FailedVerification(err)`, `Expired`,
`Evicted`, `Mined`, …) are mostly *discarded by the source* the moment they
happen. They are **not recoverable from any snapshot**. The honest guarantee is:

- **at-least-once while the stream is healthy**, and
- on any disconnect/gap, an explicit **gap marker** ("stream (re)started; you may
  have missed events") rather than a pretense of continuity. The follower
  re-snapshots to repair the *set*, but the transient events lost during the gap
  are gone — and we say so.

*Partial recovery nuance:* the mempool's rejection caches
(`tip_rejected_exact`, `tip_rejected_same_effects`,
`chain_rejected_same_effects`) retain recent failures **with their errors** for
a TTL. Folding recent rejections into the snapshot makes even "why it failed"
partially recoverable — a cheap win toward the strong bar.

## 4. The transaction lifecycle model

The wire vocabulary is the *transaction's* lifecycle, not mempool internals:

```
   submitted / gossiped
          │
          ▼
      ┌─ Queued ─┐   stage: AwaitingDownload → AwaitingVerification
      │          │                 ▲
   verified   rejected             │ tip reset re-queues verified txs
      │          │                 │
      ▼          ▼                 │
 InMempool   Removed(reason) ──────┘
      │
      └──► Removed(reason)
```

Three event kinds; all richness pushed into a typed reason:

```text
Queued  { txid, stage }                       // stage = AwaitingDownload | AwaitingVerification
Added   { tx content, fee/sigop/ZIP-317 metadata }
Removed { txid, reason }

enum RemovedReason {
    FailedDownload,
    FailedVerification(TxVerificationError),   // the specific error Zaino wants
    Mined { block },
    Expired,
    Evicted,                                   // ZIP-401 cost-limit eviction
    Reorged,                                   // tip reset; re-queued for re-verification
}
```

`stage` gives "where in the pipeline" without one event kind per internal queue.
A tx emits `Queued{AwaitingDownload}` → `Queued{AwaitingVerification}` → `Added`
or `Removed`. Apply is idempotent, so repeats/overlap are harmless.

## 5. Wire protocol (indexer gRPC)

A single streaming RPC that bootstraps then follows — purely incremental, no
fat snapshot type. The server subscribes to the mempool change broadcast
*before* replaying current state, so the bootstrap/stream race is solved locally
by single-task ordering (events that land during the bootstrap burst are applied
idempotently over it — the k8s WatchList / Netflix DBLog watermark idea).

The wire unit is a **batch**, not an individual event: one `MempoolBatch` per
mempool change cycle (`Mempool::poll_ready`). If 10 txs are verified in one
cycle, the source sends *one* message carrying all 10 events plus the single
post-batch checksum — never 10 messages.

```proto
rpc SyncMempool(Empty) returns (stream MempoolBatch);

message MempoolBatch {
  // Every lifecycle event observed in this change cycle, in order. The follower
  // applies them atomically (all-or-nothing) before checking the checksum.
  // Includes a Reorg marker event for tip resets (§5a).
  repeated MempoolEvent events = 1;

  // Hash of the follower's full replica state (verified + queued sets, §3a-1)
  // AFTER applying this batch. The follower recomputes and compares; mismatch →
  // re-bootstrap. Omitted on intermediate bootstrap chunks (see below).
  optional bytes checksum = 2;

  // True on the final bootstrap batch: the preceding batches replayed current
  // state; live cycles follow. (k8s SendInitialEvents bookmark.)
  bool initial_sync_complete = 3;
}
```

Sequence: bootstrap batch(es) replaying current state (chunked under the 4 MiB
limit; `initial_sync_complete=true` + `checksum` on the last) → live
`MempoolBatch` per change cycle, each carrying its events and post-batch
checksum. A checksum mismatch is the gap detector → re-bootstrap; there is no
separate set gap-marker. For the transient *observation* layer (§3b), a
reconnect/re-bootstrap implicitly marks a possible hole — events lost during a
gap are unrecoverable and reported as such.

Source-side loss detection: the server's own subscription is a
`tokio::broadcast` receiver, which at capacity drops the oldest message and
signals a lagging consumer with `RecvError::Lagged(n)` (n = skipped count) on
the next `recv`. The server treats its own `Lagged` as "I missed events" → drop
the client connection → re-bootstrap; the next `Checkpoint` is the backstop that
catches it regardless. (Verified round-2: this is the exact intended hook.)

Backpressure / slow consumer: each follower gets its **own bounded `mpsc`**
response channel (the bootstrap burst goes here, *not* into the shared
broadcast, so one follower bootstrapping never evicts live events for others).
On overflow → **drop the connection** (etcd "drop-and-resync") → re-bootstrap.
Large bulk removals (reorg, §5a) are id-only and small; a large re-verification
refill streams as individual `Added` events, so no single frame approaches
gRPC's 4 MiB limit (the etcd #9294 wall).

Concrete tonic/tokio config (round-2; pin & re-verify against our versions):
size the source broadcast capacity to the largest tolerable burst (so transient
hiccups don't force needless re-bootstraps); **enable HTTP/2 keepalive**
(`http2_keepalive_interval`, off by default) to detect dead followers; raise the
initial HTTP/2 window or enable `http2_adaptive_window` (default 65,535 bytes /
off) for the bursty bootstrap.

### 5a. Reorg handling

On a chain tip reset the source emits a `Reorg` signal, then streams the
re-verification results as they happen. The follower, on `Reorg`, moves its
verified set back to a re-verifying state (content is retained — the source
re-queues for *verification*, not re-download), then applies the incoming
`Added` (re-verified) / `Removed{reason}` (no longer valid) events. A
`Checkpoint` after re-verification settles convergence. This matches the
source's `TipAction::Reset` semantics exactly and never exposes a tx the source
has dropped.

Fan-out is free: each follower is an independent `SyncMempool` stream; the
server already multiplexes the mempool broadcast. No shared follower state.

## 6. Components and where the code lives

The scaled-back lifecycle keeps instrumentation to the meaningful transitions.

**Source side — `zebrad/src/components/mempool/` (the bulk of the new surface;
today nothing upstream of verification is observable):**

- `mempool_change.rs` (`zebra-node-services`): replace `MempoolChangeKind`
  (`Added`/`Invalidated`/`Mined`) with the lifecycle model `Queued{stage}` /
  `Added` / `Removed{reason}`; carry the verification error in
  `FailedVerification`.
- `downloads.rs`: emit `Queued{AwaitingDownload}` on entry,
  `Queued{AwaitingVerification}` on download completion, `Removed{FailedDownload}`
  on download failure. (2 points)
- verifier path: `Removed{FailedVerification(err)}` (1 new; `Added` exists).
- `storage.rs`: split today's `Invalidated` into `Removed{Expired}` /
  `Removed{Evicted}`.
- `mempool.rs`: `Removed{Mined}` (exists as `Mined`) + `Removed{Reorged}` at
  `TipAction::Reset` (1 new).
- New mempool tower `Request` to enumerate the queued set (+ stages) and recent
  rejection-cache entries, alongside the existing `FullTransactions`, for the
  **bootstrap burst** (replayed as events, not a snapshot blob).
- A **digest** the source computes cheaply for each batch: a hash over the full
  replica projection — verified set + queued set `{txid → stage}` (§3a-1) — at the
  settled end of the change cycle. Emitted *in* the `MempoolBatch` (§9.8), so
  divergence is caught within a single batch. Excludes source-internal machinery
  and the transient observation layer.

**Indexer server — `zebra-rpc/src/indexer/`:**

- `IndexerRPC` gains a mempool tower-service handle (today it holds only the
  `MempoolTxSubscriber`); wired in `zebrad/src/commands/start.rs`.
- `SyncMempool` handler: subscribe → snapshot (verified + queued + recent
  rejections) → forward lifecycle events; on `Added`, fetch content from the
  **local** mempool service (in-process, microseconds — the old per-add
  *network* round-trip was the latency killer).

**Follower — `zebra-rpc/src/mempool_sync.rs`:**

- `TrustedMempoolSync`: connect → apply the bootstrap burst → apply live events
  idempotently → publish. Replica keyed by full **`UnminedTxId`**. Two
  first-class outputs for Zaino:
  - a `watch<Arc<MempoolReplica>>` for the queryable queued+verified sets, and
  - a lifecycle **observation feed** (the events).
- On each `MempoolBatch`: apply its events atomically, recompute the digest over
  the local projection, compare to the batch's checksum; mismatch → reconnect →
  re-bootstrap. Stream break/timeout/`Lagged` → backoff → reconnect. No per-event
  full-map clone (a flaw of the first attempt).

## 7. Prior art (deep-research, 25/25 claims verified)

The canonical blueprint is the **k8s client-go informer**: list (consistent
snapshot capturing a starting version) + watch (incremental, totally-ordered,
resumable) + reconcile (atomic relist diff that synthesizes deletions). etcd's
monotonic `revision` is the reference cursor; an explicit "too old" error
(`410 Gone` / `ErrCompacted`) deterministically forces a full resync.

What we deliberately **cut**, justified by our constraints:

- **No version cursor / compaction window / partial resume.** etcd/k8s carry
  these to avoid re-listing a *large* dataset. Our pool is small, so a full
  re-bootstrap on divergence costs little.
- **No CRDTs, no structured set reconciliation (Merkle / negentropy / IBLT).**
  The research confirms these are "largely overkill for a single authoritative
  source." We use the *cheapest* anti-entropy instead: a single whole-set
  **checksum** per checkpoint (mismatch → full re-bootstrap), not a diffing
  reconciliation protocol.

What we **keep**: bootstrap-fused-with-stream (single-task ordering), incremental
idempotent apply, a whole-set checksum for active divergence detection, and
drop-and-resync backpressure.

The one thing the prior art does *not* cover and we add: the irrecoverable
**transient-observation layer** (§3b) on top of the recoverable set — k8s/etcd
are pure set-replication; our lifecycle reasons need at-least-once + gap markers
because the source discards them.

**Round-2 validation (focused on this design; 25/25 claims verified):**
- Digest → sorted-ids hash now, LtHash only if profiling demands (§9.6).
- Whole-set checksum + repair is an *authoritative* convergence primitive
  (Cassandra Merkle anti-entropy "guarantees eventual consistency" vs
  best-effort read-repair); the Merkle tree collapses to one root digest for a
  small set.
- `tokio::broadcast` `Lagged(n)` is the verified drop-and-rebootstrap hook;
  tonic keepalive off + 65 KB window + 4 MiB max are the defaults to override.
- Blockchain mempool-sync practice (Bitcoin flooding, Erlay/BIP330 PinSketch,
  Ethereum eth/68 push-to-√peers-else-announce) is shaped by *many untrusted
  peers + bandwidth* — constraints we don't share — so it does **not** argue
  against push-content + checksum. Ethereum's proactive push to a peer subset is
  positive evidence for our single-source→trusted-follower push.
- Sole flagged gap: the bootstrap→live boundary apply (§10) — addressed there.

## 8. Implementation plan (commit series; local only, no public push)

Each commit is self-contained, builds, and is individually reviewable.

1. `feat(mempool): model the tx lifecycle in MempoolChange` — replace
   `MempoolChangeKind` with `Queued{stage}` / `Added` / `Removed{reason}`; thread
   the verification error into `FailedVerification`; re-point the 3 existing send
   sites. Internal only. (~350)
2. `feat(mempool): emit queued + failed-download + reorg lifecycle events` — the
   2 `downloads.rs` points + `Removed{Reorged}` at `TipAction::Reset`. (~250)
3. `feat(mempool): bootstrap-state request + set digest` — tower `Request`
   returning queued txids/stages + verified set + recent rejection-cache entries
   (replayed as the bootstrap burst), plus a cheap order-independent set digest
   for checkpoints. (~250)
4. `feat(rpc): SyncMempool indexer gRPC (bootstrap + lifecycle stream +
   checkpoints + reorg)` — proto + server handler + mempool-handle plumbing;
   `Lagged`/backpressure → drop-and-resync. (~450)
5. `feat(rpc): TrustedMempoolSync follower` — incremental apply → derived
   queued+verified sets (watch) + lifecycle observation feed; checkpoint compare
   → re-bootstrap on mismatch. (~500)

Tests threaded through each; an integration test in the last. Roughly
2–2.5k LOC total.

## 9. Resolved decisions

1. **Replica key:** full **`UnminedTxId`** (exact unmined semantics; carries the
   auth digest). *Resolved.*
2. **Verification-error fidelity:** a **stable string/code**, not the structured
   `TxVerificationError` — the consumer is the same crate as the server, so a
   stable code avoids coupling the proto to consensus error shapes. *Resolved.*
3. **No fat snapshot.** State is built **incrementally**; the per-batch
   **checksum** carried in each `MempoolBatch` is just a divergence detector,
   with full re-bootstrap only on mismatch. (Reframes §3a/§5.) *Resolved.*
4. **Retire** the existing hash-only `MempoolChange` RPC once `SyncMempool`
   lands. *Resolved.*
5. **Reorg:** the source signals a reorg, then **streams re-verification results
   as they happen** (§5a); the follower re-verifies in place (content retained).
   *Resolved.*

6. **Checkpoint digest:** a **plain hash over the full replica projection**
   (verified set + queued set `{txid → stage}`, §3a-1), recomputed per batch.
   With per-batch embedded checksums the queued set is safe to include (no
   propagation-lag false-mismatch); it gives the queued state the same strong
   anti-entropy as the verified set, and is cheap. Confirmed by research
   (round 2): for a hundreds-to-thousands set a recompute is microseconds and
   trivially correct; the homomorphic multiset hashes (MuHash/ECMH/LtHash) only
   pay off when updates must be independent of set size. Naive XOR/additive-sum
   are rejected (multiset cancellation, "add same element twice"). **Upgrade
   path:** LtHash (Meta Folly; ~2 KB digest, ≥200-bit, O(1) add/remove) *iff*
   profiling shows per-batch recompute is material. *Resolved.*
7. **Repair on mismatch:** full reconnect/re-bootstrap. Cheap because the set is
   small (the reason a single root digest suffices instead of a Merkle tree to
   localize the diff). A keys-only diff pass is a possible later bandwidth
   optimization, not v1. *Resolved.*
8. **Checksum cadence:** carried **in every `MempoolBatch`** — one batch per
   mempool change cycle (`poll_ready`), the checksum computed once the cycle's
   changes have settled into a consistent state — **not** on a timer. This bounds
   undetected divergence to a single batch (the tightest bound) and stays cheap:
   the digest is over a small set and coalesces a whole cycle's changes into one
   message + one hash. Computing it at the cycle boundary makes it a consistent
   point-in-time read, never a mid-batch state, and lets the follower verify
   atomically after applying the batch. *Resolved.*

### Still open (tuning, not architecture)

- **Coalescing under pathological churn** — cadence is resolved (one checkpoint
  per verified-storage batch, §9.8). The only knob left is whether to coalesce
  several back-to-back batches into one checkpoint under extreme load; that is a
  load valve, not a cadence change. Tune empirically.
- **Digest content** — full replica projection (verified set + queued
  `{txid → stage}`) is the decision (§3a-1). Whether to also fold in tx content
  hashes / verified metadata is the only remaining knob, and it's almost
  certainly unnecessary: a same-id-different-bytes corruption can't arise from a
  trusted source. (The *scope* is settled, not open.)

## 10. Bootstrap → live boundary (the one correctness-sensitive part)

Round-2 research flagged this as the only pillar without external citation — it
rests on our own reasoning, so it gets spelled out and defended here.

**Construction:**
1. The server **subscribes to the mempool change broadcast *before*** taking the
   snapshot read, so no change is lost in the gap between read and subscribe.
2. The snapshot is a **consistent point-in-time read** (Zebra's mempool tower
   service handles one request at a time, so `FullTransactions` + the queued-set
   request observe a single coherent state).
3. Bootstrap events are replayed, then live events forwarded. Apply is
   **idempotent** (add-existing = update/no-op; remove-absent = no-op).

**The race it must survive:** a tx whose snapshot state is *later* than a live
event still buffered from *before* the snapshot read. E.g. snapshot caught X as
`InMempool`; a stale `Queued{X}` from before the read is still in the live queue
and, applied after bootstrap, would regress X `InMempool → Queued`.

**Defense (two options):**
- **(v1, simplest) lifecycle-monotonic apply + checksum backstop.** Apply never
  regresses a tx to an earlier stage *within its current generation*
  (`Queued{AwaitingDownload} < AwaitingVerification < InMempool`); a `Removed`
  starts a new generation (a txid may legitimately re-appear after eviction/
  reorg). This drops the stale `Queued{X}`. Any residual boundary race is caught
  by the next `Checkpoint` mismatch → re-bootstrap. The backstop is therefore
  *load-bearing* in v1, which is acceptable given a small set and cheap repair.
- **(optional hardening) a bare per-change ordinal.** The source stamps each
  mempool change with a monotonic `u64` and the `InitialSyncComplete` bookmark
  reports the ordinal the snapshot reflects; the follower drops any buffered live
  event with `ordinal ≤ snapshot_ordinal`. This closes the race *without* the
  checksum backstop. **This is not the resumable version cursor we rejected** —
  there is no resume-from-ordinal and no history retention; it is only a
  bootstrap-dedup tag. Add it if we want the boundary provably race-free rather
  than backstop-healed.
