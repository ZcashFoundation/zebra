# Dandelion++ Implementation — Remaining Work

This file tracks what is left before the implementation in `zebra-network/src/dandelion/`
and `zebrad/src/components/mempool/dandelion_gossip.rs` is feature-complete.

## Phase 1 — Core types (DONE, verified 2026-07-09)

- [x] `zebra-network/src/dandelion/state.rs` — `PropagationState`, `PropagationStateMap`
- [x] `zebra-network/src/dandelion/epoch.rs` — `DandelionEpochManager`
- [x] `zebrad/src/components/mempool/dandelion_gossip.rs` — gossip task scaffold
  with epoch rotation, stem-timeout sweep, and TODO marker for unicast

## Phase 2 — PeerSet per-peer routing — DONE (2026-07-09)

**Files**: `zebra-network/src/protocol/internal/request.rs`,
`zebra-network/src/peer/connection.rs`, `zebra-network/src/peer_set/set.rs`,
`zebrad/src/components/mempool/dandelion_gossip.rs`.

Implemented as a new `Request::AdvertiseTransactionIdsToPeer(HashSet<UnminedTxId>,
PeerSocketAddr)` variant (Option A from the original plan below). `connection.rs`
sends the same wire message as `AdvertiseTransactionIds` (an `inv`, not the raw
tx — unchanged from existing behavior). `PeerSet::route_to_peer` takes the named
peer out of `ready_services`, calls it, and pushes it back to `unready_services`;
if the peer isn't in `ready_services` it fails immediately with
`PeerError::NoReadyPeers` rather than falling back to some other peer (silently
picking a different peer would defeat the point of stem routing).
`dandelion_gossip.rs` now calls this real unicast instead of falling through to
broadcast, and falls back to fluff only if the stem peer wasn't ready.

Original plan (kept for context):

**Option A** — new `Request` variant:
```rust
// in zebra-network/src/protocol/external/types.rs or similar
Request::SendTransactionToPeer {
    peer: SocketAddr,
    txids: HashSet<UnminedTxId>,
}
```
`PeerSet::call()` routes it to the matching peer (by `SocketAddr` key in
`PeerSet.ready_services`).

**Option B** — `PeerSet::broadcast_to(addr, req)` helper that bypasses the
load-balancer.

Option A is more idiomatic with the existing `tower::Service` design.

**Remaining**: an integration test in `zebra-network/tests/` exercising
`route_to_peer` against a mock `Discover` with 2+ peers (unit tests only cover
the dandelion state/epoch types so far, not `PeerSet` routing itself).

## Phase 3 — Wire up in `zebrad` — DONE (2026-07-09)

**File**: `zebrad/src/commands/start.rs`

Replaced `gossip_mempool_transaction_id` with `spawn_dandelion_gossip`.
Passes a closure that reads `AddressBook` for currently-responded outbound
peers as stem-peer candidates.  A dummy `pending::<Result<(),BoxError>>()`
future satisfies the existing `select!`/`abort` downstream logic.

## Phase 4 — Mempool `pending-stem` state — DONE (2026-07-09)

`InboundSetupData` now carries `dandelion_prop_state: Arc<Mutex<PropagationStateMap>>`.
The `zn::Request::MempoolTransactionIds` handler in `zebrad/src/components/inbound.rs`
acquires the lock, identifies active-stem txids, and strips them from the response
before replying to the remote peer.

Completed (2026-07-09):
- [x] `mempool` P2P `MempoolTransactionIds` response: filters active stem txids.
- [x] `TransactionsById` P2P (getdata) handler: drops active-stem txs, reporting
  them as `Missing` — a peer that learned a stem txid out-of-band cannot fetch
  the full tx from us before fluff.
- [x] `getrawmempool` RPC (verbose + non-verbose): filters via `active_stem_txids()`.
- [x] `getrawtransaction` RPC: returns "No information available" for active stem txids.
- [x] Indexer gRPC `mempool_change` stream (Zaino/lightwalletd): suppresses
  `StemAdded` events — stem txs are only streamed once promoted to fluff (as `Added`).
- [x] `MempoolChangeKind::StemAdded`: mempool emits `StemAdded` for locally-submitted
  txs, `Added` for peer-relayed. Gossip task routes `StemAdded` through stem,
  `Added` directly to fluff.

Remaining (lower priority / minor):
- `z_getoperationresult` and other watch-operation RPCs do not filter stem txids —
  they report the queue/verify status, not the mempool contents, so the leak is
  minor; the operation result only says "success" not "tx is in mempool".
- **Timing window**: `is_active_stem()` flips to false at exactly `STEM_TIMEOUT`
  (30 s), but the fluff-broadcast sweep only runs every `STEM_TIMEOUT_CHECK_INTERVAL`
  (5 s). So for up to ~5 s a timed-out stem tx is visible via RPC/P2P but not yet
  fluff-broadcast. Harmless (it's about to be broadcast anyway) but noted for
  completeness. Could be tightened by driving the RPC/P2P filter off the same
  `should_fluff()`-with-sweep state rather than the raw timeout.

## Phase 5 — MempoolCrawler analysis (re-scoped, subsumed into Phase 4)

The crawler (`zebrad/src/components/mempool/crawler.rs`) sends
`Request::MempoolTransactionIds` *to other peers* asking for their mempool
contents — it does not expose our own stem-phase transactions.  No changes
needed here.

The inbound `zn::Request::MempoolTransactionIds` handler (Phase 4 above) is now
filtered.

## Phase 6 — Tests — DONE (2026-07-09)

- [x] Unit tests: `state.rs` (6 tests) and `epoch.rs` (4 tests) `#[cfg(test)]` modules.
- [x] Routing unit tests: `zebra-network/src/peer_set/set/tests/vectors.rs`
  `dandelion_route_to_peer_unicast` and `dandelion_route_to_peer_fails_when_not_ready`.
- [x] Phase 4 filter unit tests: `state.rs` `phase4_filter_strips_active_stem_keeps_fluff_and_unknown`
  and `phase4_filter_shows_tx_after_fluff_promotion` — directly verify the
  predicate used in `Inbound::call(MempoolTransactionIds)`.
- [ ] Full P2P integration test (spawn PeerSet + inbound handler, assert stem/fluff
  timing) — deferred; requires a substantial test harness.
- [ ] Proptest: uniform stem-peer distribution over epochs — deferred.

## Phase 7 — ZIP companion — DONE (2026-07-09)

Filed as [zcash/zips#1329](https://github.com/zcash/zips/pull/1329):
**ZIP 327: Dandelion++ Transaction Propagation for Zcash P2P Nodes**.

Covers: epoch management, per-epoch stem-peer selection, fail-closed unicast,
30 s stem timeout, stem-phase mempool filtering, fluff-phase broadcast.
Does not require a new P2P message type.

Wallet-side direct P2P submission (Component A / ZIP 328) is in
`y4ssi/zip-dandelion-direct` (private, PR #1 open).

## Wire-level stem signalling (unadvertised `tx` convention) — future work

Per ZIP 327 §Stem-phase forwarding and the draft wallet ZIP, the stem peer
SHOULD receive the raw `tx` message directly (without a prior `inv`) as a
signal that the transaction is in stem phase.

Current state: `connection.rs` sends the same `inv` for both
`AdvertiseTransactionIds` and `AdvertiseTransactionIdsToPeer`.  The privacy
routing property still holds (only the stem peer is notified), but the
downstream node cannot distinguish stem relay from normal relay.

Implementing the unadvertised-`tx` convention requires a new request variant
`AdvertiseStemTransactionToPeer(Arc<Transaction>, PeerSocketAddr)` (carrying
the full transaction, not just the id), a new code path in `connection.rs`
that sends `Message::Tx(tx)` without a prior `Inv`, and a matching receive
path in the inbound handler that treats an unrequested `Tx` message as a
stem-phase submission.

This is marked OPTIONAL in ZIP 327 and deferred until at least one other
Zcash node implementation is ready to recognize the signal.

## Known blockers / risks

1. **PeerSet API surface** — `ready_services` is private.  Extending it for
   per-peer routing will touch the `tower::Service` trait bound and may require
   a `ServiceMap` abstraction.  Estimated: 1–2 weeks.

2. **No ZIP yet** — ZF reviewers will not merge without a companion ZIP or at
   minimum a `Discussions-To` issue.

3. **`rand` dependency in `zebra-network`** — `epoch.rs` uses `rand::thread_rng`.
   Check if `zebra-network` already depends on `rand`; if not, add it to
   `zebra-network/Cargo.toml`.
