# Known-Hash Sync Engine Port Plan (generated 2026-08-17)

Produced by the map-sync-engine-port workflow (wf_c6043780-97d); inputs: branches ibd-engine, ibd-utxo-write-elision, v2-stack.
NOTE: decision 9 (design doc rev 4) postdates this plan: production verification is the pinned MaxCheckpoint bitmap hash; the salted aggregate is an opt-in verification mode only.

# Port plan: known-hash IBD engine + SwiftSync hints onto the v2 stack

Verified against: `v2-stack` (HEAD 3f8b9d985), `ibd-engine` (HEAD 3eb00dcc7), `ibd-utxo-write-elision` (HEAD 5ddad16cd). All cited paths confirmed to exist on the named branch.

## Stack commit map (v2-stack, base 529bfc8ac on main)

| id | commit | subject |
|----|--------|---------|
| c0 | 3f2cd0c92 | AddressBook/CandidateSet as tower services (precursor to c1) |
| c1 | 3d491d8c9 | peer book actor |
| c2 | 2a47babb1 | v2 wire formats |
| c3 | 04cbce21c | state sync metadata — **absorbs combined 28.1.0** |
| c4 | 44bcc4587 | QUIC transport + connection |
| c5 | b7d727cbc | dialing + endpoint |
| fixes | 4df17bc6c..3f8b9d985 | fold into c1–c5 on the next stack rebuild |
| c6–c10 | new | follow-on commits defined below |

Stack-targeted items (W1–W3) are amendments folded into c3/c4/c5 during one rebuild pass (edit c3 → rebase c4..fixes → edit c4 → rebase → edit c5 → rebase). Follow-on items (W4+) are plain new commits on the rebuilt tip.

---

## W1 — Combined 28.1.0 format (target: c3)

The only 28.1.0 in existence is v2-stack's `sync_meta_by_height`; ibd-engine's 28.1.0 (`known_hash_chunk`) collides. Neither is released, so rewrite c3 as ONE combined bump: sync-metadata CF + known-hash chunk CF + per-height cumulative transparent-output count.

**Files to modify (all on v2-stack shapes):**
- `zebra-state/src/constants.rs` — keep `DATABASE_FORMAT_MINOR_VERSION = 1` (line 59); rewrite the doc comment to describe all three additions.
- `zebra-state/src/service/finalized_state/disk_format/chain.rs` — append `cumulative_transparent_outputs: u64` to `SyncMetadata` (56 → 64-byte LE record). Update `IntoDisk` (`Vec::with_capacity(56)` → 64) and `FromDisk` (`len >= 56` assert → 64) **together** — `FromDisk` reads a fixed layout. Extend `SyncMetadata::for_block(...)` to take the previous cumulative (or a new `for_block_with_prev`).
- `zebra-state/src/service/finalized_state/zebra_db/chain.rs` — in `prepare_chain_value_pools_batch` (writes at ~line 334): read `db.sync_metadata(prev_height)` for the prior cumulative (finalized commits are strictly sequential, so height−1 is always readable), add `sum(tx.outputs().len())` over the block. Genesis: seed from 0 and **decide whether genesis's output counts toward the ordinal — must match the SwiftSync spec's output-ordinal definition** (Zebra skips genesis transparent indexes elsewhere; document the choice in the struct doc).
- **New file** `zebra-state/src/service/finalized_state/zebra_db/known_hash.rs` — copy from `ibd-engine:zebra-state/src/service/finalized_state/zebra_db/known_hash.rs` (CF `KNOWN_HASH_CHUNK`, u32 → raw chunk bytes). Register the module in `zebra_db.rs`. Copy only the CF + raw read/write handles here; the read-or-generate logic moves to W6.
- `zebra-state/src/service/finalized_state.rs` — append the chunk CF name to `STATE_COLUMN_FAMILIES_IN_CODE` (~line 109, where SYNC_META was appended).
- `zebra-state/src/service/finalized_state/disk_format/upgrade/add_sync_metadata.rs` — thread a `mut cumulative` through the height loop (lines ~48–73); seed from the resume read at lines ~42–46 (it already fetches the last `(Height, SyncMetadata)` — the seed is free). Chunk CF is **not** backfilled by the upgrade (chunks are generated on demand / persisted by the consume path), so `validate()` stays sync-meta-at-tip. Keep the inline `try_recv` cancel pattern — do NOT reintroduce the shared `check_cancelled` helper that 2fc405d60 removed.
- Snapshots: `zebra-state/src/service/finalized_state/disk_format/tests/snapshots/column_family_names.snap` + all seven `empty_column_families@{no_blocks,mainnet_0..2,testnet_0..2}.snap` — add `known_hash_chunk` lines (regenerate via `cargo insta` / test run, don't hand-edit blindly).

**Copy vs fresh:** known_hash.rs copied near-verbatim; everything else is edits to v2-stack files. **Risk to flag:** any local dev DB already stamped 28.1.0 will never re-run the extended upgrade — delete such DBs (user's machines only; unreleased format).

## W2 — v2 client-side sync/artifact requests (target: c4)

The server side of get-hashes/get-tree-roots/get-object is complete on v2-stack; the client side is the gap.

**Files to modify:**
- `zebra-network/src/protocol/internal/request.rs` — internal `Request::SyncHashes`/`TreeRoots` (lines 260/281) are local-only by contract; add **distinct peer-routable variants** (suggested: `RemoteSyncHashes { start_height, stride, count }`, `RemoteTreeRoots { start_height, final_hash, count }`, `Object { hash: ObjectHash, offset: u64, length: u64 }`). Give all three `PeerCapability::V2` in `Request::peer_capability` (~line 306–322).
- `zebra-network/src/protocol/internal/response.rs` — reuse existing `Response::SyncHashes`/`TreeRoots` for the remote variants; add `Response::Object { total_size: u64, bytes: Vec<u8> }`.
- `zebra-network/src/peer/v2/connection.rs` — in `drive_outbound_request` (~1497): map `RemoteSyncHashes` → `WireRequest::GetHashes` decoding via `HashesResponse::read`, `RemoteTreeRoots` → `GetTreeRoots` via `TreeRootsResponse::read` (drop the `#[cfg_attr(not(test), allow(dead_code))]` at `protocol/v2/response.rs:320/365` — these decoders were written for exactly this). Write a **new** client reader for the get-object framing (RESULT byte + CompactSize total size + raw bytes — server encoder at ~2507 has no reader anywhere; mirror it, bound by `MAX_GET_OBJECT_LENGTH` = 32 MiB). Keep the local-only rejection (~1703) for the old `SyncHashes`/`TreeRoots` names.
- `zebra-network/src/peer/connection.rs` (legacy v1) — reject the three new variants with `PeerError::LocalOnlyRequest`-style unroutable handling (they are v2-only; the `PeerCapability::V2` filter should prevent routing, but the match must stay exhaustive).
- Routing for `Object`: peers without artifact stores answer Refused. Cheapest correct option: tolerate `Refused` → retry on another peer (already retryable per the BlockRange precedent, connection.rs ~1449). Optional refinement: a `PeerCapability::V2Artifacts` that checks `NODE_SYNC_ARTIFACTS` in the client's `remote_init.services` inside `select_ready_p2c_peer` (`peer_set/set.rs:917–933`) — watch/atomic state only, **no mutexes**.

**Copy vs fresh:** all fresh code, but small — wire codecs, server, error mapping all exist. Transport does NOT verify object content; callers (W8) SHA-256-verify.

## W3 — PeerSetStatus watch (target: c5)

- Copy from `ibd-engine:zebra-network/src/peer_set/set.rs` — `PeerSetStatus { ready_peers }` (line 181), `status_sender: watch::Sender` (line 396), channel creation (~472), `status_receiver()` (~527), publish call (~1871) — re-derive placement against v2-stack's diverged `set.rs` (v2 capability filtering, peer book actor). Re-apply intent, not diff.
- Export from `zebra-network/src/lib.rs` (ibd-engine exports it at line 194) and return the receiver from `peer_set/initialize.rs` `init()`.
- Consider publishing a per-capability count (`ready_v2_peers`) since the engine will fetch only over v2 — one extra field now avoids a second watch later.
- Already watch-based; conforms to the no-mutex rule.

## W4 — Two-tier state write pipeline (new commit c6)

Prerequisite for the engine's commit-ack model (ack when in-memory, disk trails).

**Files:**
- **New:** `zebra-state/src/service/write/worker.rs`, `write/disk_writer.rs`, `write/rpc_indexer.rs` — copy from ibd-engine (verified mutex-free). Module layout note: user rule is `write.rs` + `write/` dir — that is exactly ibd-engine's shape.
- **Rework:** `zebra-state/src/service/write.rs` (v2-stack has only this single file), `zebra-state/src/service/finalized_state.rs`, `zebra-state/src/service.rs` — port the ibd-engine split by re-applying intent; v2-stack carries NU6.3/Ironwood-era code the ibd-engine base (b36a3ed59) predates.
- `zebra-state/src/service.rs` — `init` signature: rename/extend `max_checkpoint_height: block::Height` (v2-stack service.rs:315) to `max_finalizable_height` per `ibd-engine:zebrad/src/commands/start.rs:355–378`; update `zebrad/src/commands/start.rs` call site (v2-stack still passes max_checkpoint_height).
- `zebra-state/src/config.rs` — add `separate_rpc_index_db`, `disable_wal_during_ibd`, `checkpoint_sync_retained_blocks`, `checkpoint_sync_pipeline_capacity` from ibd-engine.
- Preserve `CommitCheckpointVerifiedBlockRequest`/`MappedRequest` (present on both branches, request.rs:694 v2-stack / :846 ibd-engine) — extend to carry optional `SuppliedTrees` as on ibd-engine.
- **Do NOT port** `tower-fair-buffer` yet: it uses `std::sync::Mutex` throughout (`future.rs`, `queue.rs` — verified) and violates the hard no-mutex rule. The engine runs behind the existing peer-set buffer initially; fair queueing is a separate later work item requiring a mutex-free rewrite.
- Re-verify the §4.6 deterministic-commit-failure/reset-recovery logic against v2-stack state code after the merge — do not assume it survives.

## W5 — Known-hash data formats + asset crates (new commit c7, or merged into c8)

- Copy near-verbatim: `zebra-chain/src/parameters/known_hashes.rs` + `known_hashes/chunk_v2.rs` from ibd-engine; register `pub mod known_hashes;` in `zebra-chain/src/parameters.rs` (ibd-engine line 18).
- Copy: `zebra-known-hashes/` umbrella + chunk crates (publish=false), `zebra-utils/src/bin/known-hashes/main.rs`, workspace `Cargo.toml` members.
- Keep chunk_v2 tree-root sections sapling/orchard-only (Ironwood fits later via reserved flag bits).
- Ship with the existing pinned constants (Mainnet max 3,373,206 — note `ibd-engine` HEAD, and `ibd-utxo-write-elision` commit 8d31e52af, extend the Mainnet list; prefer the ibd-utxo-write-elision 8d31e52af version of `known_hashes.rs` if newer). Re-sweep with the known-hashes tool is a release task, not a port blocker.

## W6 — State chunk request plumbing (same commit as W5 or c7b)

- `zebra-state/src/request.rs` — copy `Request::KnownHashChunk(u32)` (ibd-engine :1202), `Request::WriteKnownHashChunk` (:1216), `ReadRequest::KnownHashChunk` (:1663); skip `ReadRequest::IsTransparentOutputSpent` (superseded by SwiftSync hints).
- `zebra-state/src/response.rs`, `zebra-state/src/service.rs` — the matching Response variants + dispatch (ibd-engine service.rs ~1323–1335 write via spawn_blocking, ~1879 read-or-generate).
- `zebra-state/src/service/read/snapshot.rs` — copy only `known_hash_chunk_bytes` (chunk_v2 re-encode of a 150k span incl. tree-root updates); leave the rest of the artifact-emission module to W10.
- Serving safety: mirror `read::sync_hashes`'s `tip − MAX_BLOCK_REORG_HEIGHT` bound for chunk generation.

## W7 — Engine + supervisor, legacy syncer retained (new commit c8)

**Copy near-verbatim from ibd-engine (all verified, all mutex-free):** `zebrad/src/components/ibd.rs`, `ibd/engine.rs`, `ibd/fetch.rs`, `ibd/cache.rs`, `ibd/convert.rs`, `ibd/semantic.rs`, `ibd/discovery.rs`, `ibd/tree.rs`, `ibd/consume.rs`, `ibd/consume/local.rs`, `ibd/embedded_assets.rs`.

**Key bridge facts (verified):**
- `Request::BlocksByHash` works over v2 unchanged — v2 `drive_outbound_request` maps it to `WireRequest::GetBlocks` (v2-stack connection.rs:1542–1584). **FetchBatcher ports as-is**; revisit `IBD_BATCH_MAX_BLOCKS=16`/weight constants against QUIC semantics later (optionally switch bulk fetch to `Request::BlockRange` — fully working e2e, client :1622 / server :2378, free hash-chain+merkle verification — as a follow-up experiment, respecting `MAX_CONCURRENT_BULK_STREAMS=2`/peer and `REQUEST_TIMEOUT` per whole response).
- `convert.rs` needs `zebra_consensus::spawn_fifo` + `merkle_root_validity` re-exports — check they exist on v2-stack's zebra-consensus; add re-exports if not (do NOT delete checkpoint.rs/router.rs in this commit).
- `zebrad/src/commands/start.rs` — wire per ibd-engine: `max_finalizable_height` into init (~355, done in W4), `ibd_cache_dir` (~453), `ibd::commit_genesis_if_missing` (~695), `IbdEngine::new(..., peer_set_status, ...)` (~699), `ibd::spawn_engine_then_tip_sync(ibd_engine, syncer.sync())` (~708). **Keep v2-stack's legacy `sync.rs`/`downloads.rs` and checkpoint verifier**: engine runs to list max via `CommitCheckpointVerifiedBlock` (bypassing consensus), then hands off to the legacy syncer. This defers ibd-engine's invasive sync.rs rewrite.
- `[sync]` config fields (`known_hash_sync`, `known_hash_lookahead_bytes`, `known_hash_gap_hedge_secs`, `known_hash_tree_lookahead`, `known_hash_list_dir`, `known_hash_local_source_dir`) — copy from `ibd-engine:zebrad/src/components/sync.rs:264–402` into v2-stack's sync Config without the syncer rewrite. Skip `snapshot_consume_sync` (superseded).
- Skip porting: `zebra-state/src/snapshot_consume.rs`, `ibd/tree.rs` snapshot-consume wiring beyond compilation, `[state] snapshot_consume` config — SwiftSync (W9) supersedes SurvivorSet; `unspent_outputs_hash`/`address_balances_hash` were never pinned. Keep `SuppliedTrees`/tree-lookahead types compiling (they're the hint-payload seam W9 reuses).
- Expect textual conflicts in request/response variant lists and service dispatch from upstream drift (#11218 etc.) — re-apply intent.

## W8 — v2-native hash discovery + chunk distribution (new commit c9)

- Feed `CfHashSource` (`ibd/consume.rs` verify-then-`WriteKnownHashChunk`-then-cache) from the network instead of only `LocalSnapshotSource`: fetch chunk artifacts via W2's `Request::Object` (piece ≤ 32 MiB, SHA-256-verified against `KnownHashListSpec` pins before persisting), falling back to bundled/embedded assets. Writing fetched artifacts into `cache_dir`'s artifact dir makes the node serve them (GetObject server + `NODE_SYNC_ARTIFACTS` advertisement at `peer_set/initialize.rs:263` already exist).
- Tip-following discovery: replace or augment `DiscoverySource`'s legacy crawl feed with `Request::RemoteSyncHashes` (strided hashes + span aggregates give size hints for free — exactly what `FetchRequest.size_hint` wants). This is the "Phase 5 v2 synchronization requester" the dead-code readers were reserved for; keep it aligned with project_p2p_v2_plan phases.
- Optionally port the ibd-engine sync.rs rewrite (`Engine::new_semantic` + DiscoverySource per sync_cycle, delete `sync/downloads.rs`) and the checkpoint.rs/router.rs → `checkpoints.rs` `CheckpointGateLayer` replacement **as its own commit after the engine is proven** — this is the most invasive slice; it is deliberately last among non-hint work.

## W9 — SwiftSync spentness hints (new commits c10a/c10b; task #13)

**c10a — write-side elision interface (from ibd-utxo-write-elision 5ddad16cd):**
- Copy near-verbatim (rebase cleanly, upstream-shaped): `zebra-state/src/service/finalized_state.rs` `ElisionContext` + `commit_finalized_direct_with_elision`; `zebra_db/block.rs` `lookup_spent_utxos` + `write_block` threading; `zebra_db/transparent.rs` reversible-write skip (`elided` flag on create side, `continue` on spend side); the differential test harness `finalized_state/tests/elision.rs` + `cf_raw_entries` helpers (`zebra_db.rs`, `disk_db.rs`).
- **Do NOT port** `write/elision_buffer.rs` (+tests) or its disk_writer.rs hook — the depth-window reactive detection is replaced by the hint predicate.
- Keep the kill-switch pattern: replace `checkpoint_sync_elide_window` with a hint-artifact enable/path config; `ElisionContext::none()` keeps the byte-identical default path and the differential test doubles as the "hint marks nothing spent" equivalence harness.
**c10b — hint model (fresh code, per the updated spec from task #7):**
- Bitmap artifact: whole-bitmap spentness keyed by global output ordinal = W1's cumulative transparent-output count (per-height base) + intra-block index. Distribution via `Request::Object`; pin the artifact hash next to the chunk pins in `KnownHashListSpec` (the `unspent_outputs_hash` Option is the pattern; replace it).
- Salted keyed-BLAKE2b-256 multiset aggregates: add at create / remove at spend, verified against the artifact before the elided writes are trusted. **This is the value-coverage answer**: elided spends never read the DB, so spent-output values for value-pool/balance math must come from the aggregate scheme or an in-memory sidecar — the sharpest correctness edge of the whole port.
- Headers-first hint verification: fetch headers to checkpoint via `FindHeaders` (works over v2 today via the get-headers bridge, connection.rs:1530–1540) + `RemoteTreeRoots` for anchor checks.
- Reader semantics: hinted-spent rows are permanently absent from `utxo_by_out_loc`/`utxo_loc_by_transparent_addr_loc`; keep the unconditional append-only writes (tx links, `received` counter, cfg(indexer) spent index) exactly as the elision commit does.

## W10 — emit/release tooling (optional trailing commit)

`zebrad/src/commands/emit_snapshot.rs` + `emit_snapshot/editor.rs`, rest of `service/read/snapshot.rs`, extended to emit the spentness bitmap + aggregates. Not needed to sync; needed to cut artifacts.

---

## Dependency order

```
W1 (c3) ─┐
W2 (c4) ─┼─ stack rebuild pass (sequential: c3 → c4 → c5, fold fix commits)
W3 (c5) ─┘
W4 (c6, state write pipeline)   ← needs rebuilt stack tip only
W5+W6 (c7, chain formats + chunk plumbing) ← needs W1's CF
W7 (c8, engine) ← needs W3 (status watch), W4 (commit-ack), W5/W6 (hash source); W2 not required yet
W8 (c9, v2-native discovery/distribution) ← needs W2 + W7
W9a (c10a, elision interface) ← needs W4 (its commit path); independent of W7/W8
W9b (c10b, hints) ← needs W1 (cumulative count), W2 (Object), W9a; engine integration needs W7
W10 ← last
```

## Parallel worktrees (non-conflicting lanes)

- **Lane 1 (stack rebuild, one worktree, sequential inside):** W1 → W2 → W3. These rewrite c3–c5 history; nothing else may branch from the stack until this lands. W1 is task #11.
- **Lane 2 (zebra-chain formats + assets, parallel with Lane 1):** W5 file copies (`known_hashes.rs`, `chunk_v2.rs`, `zebra-known-hashes/`, `zebra-utils` tool) — zero overlap with Lane 1 except the one-line `parameters.rs` module registration; develop against the pre-rebuild tip and cherry-pick onto the rebuilt stack.
- **Lane 3 (hints spec code, parallel with everything):** the keyed-BLAKE2b multiset aggregate primitives + bitmap artifact encode/decode as a fresh zebra-chain module (W9b core) — pure new code, no branch dependencies.
- **Lane 4 (after Lane 1 lands):** W4 (touches `zebra-state/src/service/write*`, `service.rs`, `finalized_state.rs`, `config.rs`, `lib.rs`) in one worktree, in parallel with W6 (touches `request.rs`, `response.rs`, `read/snapshot.rs`, `zebra_db/known_hash.rs`) in another — overlap only in `service.rs` dispatch and `finalized_state.rs`; assign `service.rs` merge to whichever lands second.
- **Not parallelizable:** W7/W8 (both rewrite `zebrad/src/components` + `start.rs`); W9a conflicts with W4 in `finalized_state.rs`/disk-writer files — run after W4.

## API mismatches summary (bridge list)

1. `zebra_state::init` `max_checkpoint_height` → `max_finalizable_height` (v2-stack service.rs:315 vs ibd-engine start.rs:355) — W4.
2. `PeerSetStatus` watch absent on v2-stack — W3 (copy from ibd-engine set.rs:181/396/527/1871).
3. Internal `SyncHashes`/`TreeRoots` names taken by local-only server plumbing — new `Remote*` variants, W2.
4. `GetObject`: no internal variant, no client reader anywhere — all fresh in W2.
5. 28.1.0 collision — resolved by W1 as one combined format; local DBs already stamped 28.1.0 must be deleted.
6. `CommitCheckpointVerifiedBlockRequest` exists on both branches — extend with optional `SuppliedTrees` (W4), don't recreate.
7. `BlocksByHash` → v2 `GetBlocks` bridge already exists (connection.rs:1542) — FetchBatcher unchanged in W7.
8. `tower-fair-buffer` is mutex-based (future.rs:7, queue.rs:63 etc.) — **excluded from the port** pending a mutex-free rewrite; all other port candidates verified mutex-free.
9. `SyncMetadata` already has Ironwood fields on v2-stack; chunk_v2 stays sapling/orchard-only with reserved flags.
10. Checkpoint verifier deletion (`zebra-consensus/src/checkpoint.rs`/`router.rs` → `checkpoints.rs` gate) deferred to the optional tail of W8 — engine-first staging keeps the legacy syncer as fallback.

---

# Component maps (JSON)

```json
[
  {
    "component": "ibd-utxo-write-elision \u2014 intra-window transparent UTXO write elision on the checkpoint disk-writer thread (single commit 5ddad16cd atop ancestor 8d31e52af shared with ibd-engine)",
    "files": [
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/write/elision_buffer.rs",
        "role": "Core: ElisionBuffer \u2014 holds up to `depth` consecutive checkpoint blocks pre-commit, detects in-window create+spend pairs, emits ReadyBlock {block, bulk, elided_output_locations, pending_creates}",
        "loc": 317
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/write/disk_writer.rs",
        "role": "Hook point: disk-writer loop routes bulk fire-and-forget checkpoint blocks through the buffer; drain_buffer() choke points on any non-eligible write, EndBulk, and shutdown; commit_ready()/after_commit() commit + publish tip",
        "loc": 190
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/finalized_state.rs",
        "role": "ElisionContext {pending_creates, elided_output_locations} + ElisionContext::none(); new commit_finalized_direct_with_elision (commit_finalized_direct delegates with none())",
        "loc": 83
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/finalized_state/zebra_db/block.rs",
        "role": "lookup_spent_utxos gains pending_creates fallback (DB -> buffer -> same-block); write_block/prepare_transparent... thread elided_output_locations down",
        "loc": 72
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/finalized_state/zebra_db/transparent.rs",
        "role": "Batch builders: prepare_new_transparent_outputs_batch skips reversible inserts for elided locations; prepare_spent_transparent_outputs_batch `continue`s past elided spends; append-only rows unchanged",
        "loc": 79
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/config.rs",
        "role": "Config field checkpoint_sync_elide_window (default 0 = kill switch / byte-identical path)",
        "loc": 39
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/constants.rs",
        "role": "MAX_CHECKPOINT_SYNC_ELIDE_WINDOW = MAX_BLOCK_REORG_HEIGHT cap",
        "loc": 9
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/finalized_state/tests/elision.rs",
        "role": "Differential test: elision_matches_no_elision \u2014 byte-for-byte CF equality across create/spend/survive sequence via cf_raw_entries",
        "loc": 404
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/write/elision_buffer/tests.rs",
        "role": "Buffer unit tests: elision sets, spend resolution, flush order, drain completeness, crash-suffix invariant",
        "loc": 302
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/finalized_state/zebra_db.rs",
        "role": "test-only cf_raw_entries helper for the differential test",
        "loc": 9
      },
      {
        "path": "ibd-utxo-write-elision:zebra-state/src/service/finalized_state/disk_db.rs",
        "role": "underlying raw CF iteration helper (test support)",
        "loc": 17
      }
    ],
    "key_types": [
      "ElisionBuffer (write/elision_buffer.rs) \u2014 depth-bounded VecDeque<BufferedBlock> + three tracking maps: in_buffer_creates: HashMap<OutPoint,(OutputLocation,Utxo)> (detection), elided_pending: HashMap<OutPoint,(OutputLocation,Utxo)> (value resolution surviving the creator's flush), elided_locations: HashSet<OutputLocation> (confirmed-elided, not-yet-consumed)",
      "ElisionBuffer::push(block, bulk) -> Option<ReadyBlock> \u2014 index creates/spends, mark in-window pairs elided; evicts oldest when over depth; depth 0 = pass-through with empty sets",
      "ElisionBuffer::drain_one() \u2014 height-ordered full drain used by every write-through/EndBulk/shutdown path",
      "BufferedBlock \u2014 block + created_outpoints + elided_spends[(OutPoint, OutputLocation)]",
      "ReadyBlock \u2014 block, bulk, elided_output_locations: HashSet<OutputLocation> (both create-side and spend-side halves), pending_creates (elided creates of earlier flushed blocks this block spends)",
      "ElisionContext<'a> (finalized_state.rs) \u2014 borrowed {pending_creates, elided_output_locations}; ElisionContext::none() = OnceLock-backed empty statics",
      "FinalizedState::commit_finalized_direct_with_elision(block, prev_trees, &ElisionContext, source) \u2014 the only commit entry that elides",
      "ZebraDb::lookup_spent_utxos(finalized, pending_creates) \u2014 spent-UTXO resolution order: DB output_location/utxo -> pending_creates -> same-block new_outputs; expect() panics if all miss",
      "DiskWriteBatch::prepare_new/spent_transparent_outputs_batch(..., elided_output_locations, ...) \u2014 create side: skip utxo_by_out_loc insert + utxo_loc_by_transparent_addr_loc insert when location is elided; spend side: `continue` (skip both deletes) when elided",
      "Config::checkpoint_sync_elide_window: u32 (default 0), MAX_CHECKPOINT_SYNC_ELIDE_WINDOW = MAX_BLOCK_REORG_HEIGHT"
    ],
    "integration_points": [
      "disk_writer.rs run loop, DiskRequest::Write arm: buffer_eligible = bulk && ack.is_none() && matches!(FinalizableBlock::Checkpoint) \u2014 only fire-and-forget bulk checkpoint blocks buffer; genesis/overflow/acked/contextual writes drain the buffer first then write through with ElisionContext::none()",
      "disk_writer.rs DiskRequest::EndBulk arm: drain_buffer() before dropping the FinalizedWritePhase guard, so the on-disk set is complete before the worker's semantic commit",
      "disk_writer.rs channel-close path: drain_buffer() before guard drop (clean shutdown)",
      "after_commit(): disk_tip_height.store(Release) only after an actual DB commit \u2014 the worker's prune loop and durable tip never see a buffered block",
      "finalized_state.rs commit_finalized_direct_with_elision -> lookup_spent_utxos(finalized, elision.pending_creates) for checkpoint blocks -> write_block(..., elision.elided_output_locations, ...)",
      "block.rs write_block -> prepare_transparent_transaction_batch(..., elided_output_locations, address_balances) -> the two batch builders in transparent.rs",
      "Elision set computed relative to the whole write path: only utxo_by_out_loc and utxo_loc_by_transparent_addr_loc rows are conditional; tx_loc_by_transparent_addr_loc receiving+spending links, monotonic `received` counter, cfg(indexer) spent-output index, balance/value-pool math (computed from resolved Utxo values, not physical rows) are unconditional"
    ],
    "port_notes": [
      "DECISION MODEL: an output is elided iff its spend arrives while its creating block is still in the depth-bounded buffer \u2014 detection is reactive, via in_buffer_creates lookup at spend-index time. A hint bitmap replaces this entire detection machinery: elision becomes a pure O(1) predicate on the output's global index (per-height cumulative transparent-output count -> bitmap bit), decided at create time with whole-span knowledge, no buffering of blocks needed.",
      "REUSABLE AS-IS: the reversible/irreversible split in transparent.rs batch builders (the `elided` flag on the create side, the `continue` on the spend side) and the elided_output_locations: HashSet<OutputLocation> plumbing through commit_finalized_direct_with_elision -> write_block -> prepare_transparent_transaction_batch. This is exactly the write-side interface a hint bitmap needs; only the producer of the set changes.",
      "REUSABLE WITH CHANGE: ElisionContext \u2014 keep the shape, but for hint-driven elision pending_creates can be dropped or replaced. In the hint model 'spends never look up': lookup_spent_utxos's DB->buffer->block fallback chain is the wrong shape. But note WHY the lookup exists: the spent Utxo's value feeds the value pool and address balances. Whole-span hinted elision must get spend values another way (SwiftSync aggregates: salted multiset add at create / remove at spend, verified against the artifact) or keep values in memory/sidecar \u2014 otherwise value-pool and balance computation breaks. This is the main correctness surface for the port.",
      "REWRITE: ElisionBuffer itself (317 loc + 302 test loc) \u2014 its buffering, eviction, elided_pending resolution, and drain choke points exist only because the window is short and detection is retrospective. With hinted-spent outputs never written and hinted-unspent written blind, no blocks are held, so the crash-consistency argument changes: current design guarantees on-disk completeness up to the durable tip (crash re-downloads only the buffered tail). Whole-span elision instead leaves permanent holes in utxo_by_out_loc for hinted-spent outputs, so correctness must come from hint verification (the keyed-BLAKE2b multiset aggregate check) rather than the net-to-zero argument; a wrong hint is no longer self-correcting.",
      "HINT-MISS HANDLING: current spend side silently skips deletes only for locations it proved elided; blind whole-span mode must decide what a spend of a hinted-unspent output does (delete exists, fine) vs a spend of a hinted-spent output whose create was elided (nothing on disk, and with 'spends never look up' also no location/value from DB). The location half is derivable (creates in earlier finalized blocks need OutputLocation \u2014 currently from DB output_location(); a blind port needs outpoint->location resolution, e.g. via the cumulative-count CF plus tx index, or hint metadata).",
      "CONFIG: checkpoint_sync_elide_window u32 + cap constant generalize naturally to an enable flag / hint-artifact path; keep the kill-switch-default pattern and the ElisionContext::none() byte-identical path \u2014 the differential test (elision_matches_no_elision, cf_raw_entries) is directly reusable as the harness for hint-mode equivalence testing when the hint marks nothing spent.",
      "CONFLICTS WITH v2-stack: this commit sits on the ibd-engine write pipeline (disk_writer.rs, FinalizedWritePhase, any-order commit stack f54d04bf9/c68d9436b/4ca2c93b6) which does not exist on v2-stack; porting elision requires porting or reimplementing that disk-writer stage first, or attaching the elision predicate to whatever commit path v2-stack's port uses. The transparent.rs/block.rs/finalized_state.rs changes are against upstream-shaped files and should rebase cleanly.",
      "Also note the branch carries unrelated ibd commits below the tip (known-hash list extension 8d31e52af, any-order pipeline, peer-cache bootstrap fixes); only 5ddad16cd is the elision feature."
    ],
    "risks": [
      "Value-pool/balance correctness is the sharp edge: current design resolves elided spend values from elided_pending in memory; a 'spends never look up' whole-span port has no such source and must derive value coverage from the SwiftSync aggregate scheme or the port silently corrupts value pools.",
      "Crash consistency argument does not transfer: buffer-drain choke points make current elision state-neutral and crash-safe by construction; blind hint-driven elision makes the DB permanently diverge from the no-hint layout for utxo_by_out_loc / utxo_loc_by_transparent_addr_loc unless hinted-spent rows are also expected absent by all readers (RPC address-index queries during/after sync will see missing unspent-output rows only if a hint was wrong \u2014 wrong hints must be detected by aggregate verification before commit).",
      "cfg(indexer) spent-output index and address-transaction links are written unconditionally here; verify the v2-stack port keeps those append-only writes for hinted-spent outputs or indexer-facing data goes missing.",
      "disk_writer commit_ready panics on write error for buffered (already-acked) blocks \u2014 acceptable for a short buffer, but a whole-span blind writer amplifies the blast radius of any deferred-write error."
    ]
  },
  {
    "component": "v2 QUIC wire/client transport on branch v2-stack (zebra-network peer/v2 + protocol/v2)",
    "files": [
      {
        "path": "zebra-network/src/protocol/v2/request.rs",
        "role": "Wire Request enum for all 9 request stream types incl. GetHashes/GetBlockRange/GetTreeRoots/GetObject; encode() + async read(); bounds checks (check_get_hashes_bounds)",
        "loc": 433
      },
      {
        "path": "zebra-network/src/protocol/v2/response.rs",
        "role": "Wire response types: HeadersResponse (+check_contiguous), BlockResponseEntry, TxResponseEntry, AddrResponse, MempoolResponse, HashesResponse (Vec<SyncHashEntry>), TreeRootsResponse (Vec<TreeRootsEntry>). HashesResponse::read/TreeRootsResponse::read are #[cfg_attr(not(test), allow(dead_code))] \u2014 reserved for the 'Phase 5 v2 synchronization requester'",
        "loc": 415
      },
      {
        "path": "zebra-network/src/protocol/v2/types.rs",
        "role": "StreamType (0x00 handshake, 0x01-0x09 requests: GetHashes=0x06, GetBlockRange=0x07, GetTreeRoots=0x08, GetObject=0x09; 0x10-0x12 announcements), ObjectHash([u8;32] SHA-256), ErrorCode (Refused=0x08, Cancelled=0x07, Flood=0x05), WireError with connection_error_code()",
        "loc": 263
      },
      {
        "path": "zebra-network/src/protocol/v2/constants.rs",
        "role": "Limits: MAX_GET_HASHES_COUNT=50_000, MAX_GET_BLOCK_RANGE_COUNT=65_536, MAX_GET_BLOCK_RANGE_BYTES=64MiB, MAX_GET_TREE_ROOTS_COUNT=4_000, MAX_GET_OBJECT_LENGTH=32MiB (artifacts piece-sized to this), MAX_CONCURRENT_BULK_STREAMS=2 per peer (server side), MAX_CONSECUTIVE_REQUEST_TIMEOUTS=3, INBOUND_STREAM_TIMEOUT=2\u00d7REQUEST_TIMEOUT, MIN_CONCURRENT_BIDI_STREAMS=32, ALPN ids",
        "loc": 253
      },
      {
        "path": "zebra-network/src/protocol/v2/record.rs",
        "role": "Low-level record/CompactSize/length-prefixed read-write helpers, recv/send limit checks, expect_end_of_stream",
        "loc": 341
      },
      {
        "path": "zebra-network/src/peer/v2/connection.rs",
        "role": "Both halves of a v2 connection. Client side: Connection::run select loop (client_rx + accept_bi/accept_uni), dispatch_client_request (line 742, outbound at 828), spawn_outbound_request (1349, per-request tokio task + REQUEST_TIMEOUT), drive_outbound_request (1497: maps internal Request \u2192 wire request; BlockRange client at 1622 with hash-chain + merkle verification on arrival), send_wire_request (1799: open_bi, write type byte + request, finish send half). Server side: serve_inbound_request_stream (1832), serve_request (2078): GetBlockRange at 2378 (bulk-slot SlotCounter refusal), GetHashes 2452 \u2192 internal Request::SyncHashes, GetTreeRoots 2477 \u2192 Request::TreeRoots, GetObject 2506 (reads hex-named file from artifact_dir, streams offset/length with size header)",
        "loc": 2878
      },
      {
        "path": "zebra-network/src/peer/v2/service.rs",
        "role": "v2 Handshake tower Service: quinn::Connection \u2192 same crate::peer::Client type as legacy; wires SharedConnection (artifact_dir from config.cache_dir.artifact_dir_path, consecutive_timeouts, active_bulk_streams), spawns connection/announcer/trickle/mempool-subscription/heartbeat tasks",
        "loc": 415
      },
      {
        "path": "zebra-network/src/peer/v2/handshake.rs",
        "role": "initiate/respond init-record exchange on stream 0x00, version negotiation vs MIN_V2_PROTOCOL_VERSION, nonce self-connection detection",
        "loc": 263
      },
      {
        "path": "zebra-network/src/peer/v2/connector.rs",
        "role": "Outbound QUIC dial \u2192 HandshakeRequest for the v2 Handshake service",
        "loc": 108
      },
      {
        "path": "zebra-network/src/protocol/v2/quic.rs",
        "role": "QUIC endpoint/transport config (ALPN per network, stream/idle limits, retry-token flood defence)",
        "loc": 314
      },
      {
        "path": "zebra-network/src/protocol/internal/request.rs",
        "role": "Internal Request variants: BlockRange{final_hash,count,max_bytes} (line 238, routed to v2 peers), SyncHashes (260) and TreeRoots (281) both LOCAL-ONLY (doc: 'never routed to a remote peer'); PeerCapability::{Any,V2} + Request::peer_capability (306-322). NO internal GetObject variant exists",
        "loc": 433
      },
      {
        "path": "zebra-network/src/protocol/internal/response.rs",
        "role": "Response::SyncHashes(Vec<SyncHashEntry>) (line ~72), Response::TreeRoots(Option<Vec<TreeRootsEntry>>) (~79, None = anchor not in best chain \u2192 wire Refused)",
        "loc": 180
      },
      {
        "path": "zebra-network/src/peer_set/set.rs",
        "role": "PeerSet routing: select_ready_p2c_peer(capability) filters svc.is_v2() for PeerCapability::V2 (lines 917-933), route_p2c (1057-1058); falls back to None\u2192unroutable when no ready v2 peer; query_load is private; no public per-peer targeting API",
        "loc": 0
      },
      {
        "path": "zebra-network/src/peer/load_tracked_client.rs",
        "role": "LoadTrackedClient::is_v2 (line 62): peer is v2 iff RemoteHandshake::Init; PeakEwma load metric used by P2C",
        "loc": 0
      },
      {
        "path": "zebra-network/src/peer_set/initialize.rs",
        "role": "Wires both transports into one peer set; advertises PeerServices::NODE_SYNC_ARTIFACTS (line 263) when artifact dir exists; AddrTransports::QUIC tracks which peers are dialable over v2 (line 1547)",
        "loc": 0
      },
      {
        "path": "zebra-chain/src/block/sync_metadata.rs",
        "role": "SyncHashEntry{hash, span_size, span_txs, span_notes} and TreeRootsEntry{sapling/orchard/ironwood roots+txs, auth_data_root}",
        "loc": 0
      },
      {
        "path": "zebrad/src/components/inbound.rs",
        "role": "Inbound service answers SyncHashes (607\u2192zs::ReadRequest::SyncHashes) and TreeRoots (614\u2192zs::ReadRequest::TreeRoots); BlockRange unreachable! at 636 (peer-only request)",
        "loc": 0
      }
    ],
    "key_types": [
      "protocol/v2 Request (wire) \u2014 all 9 request stream encodings; GetHashes{start_height,stride,count}, GetBlockRange{final_hash,count,max_bytes}, GetTreeRoots{start_height,final_hash,count}, GetObject{hash:ObjectHash,offset,length}",
      "StreamType \u2014 first byte of every QUIC stream; is_request/is_announcement",
      "ObjectHash \u2014 SHA-256 content address of a sync artifact (protocol/v2/types.rs:125)",
      "HashesResponse / TreeRootsResponse \u2014 response decoders that exist but are dead_code outside tests (response.rs:320, :365) \u2014 the engine will be their first real caller",
      "internal Request::BlockRange \u2014 the ONLY v2-only request zebrad can route to a peer today; returns Response::Blocks in descending height order, Missing(final_hash) if anchor unknown, truncation = resume from last block's previous_block_hash",
      "internal Request::{SyncHashes,TreeRoots} \u2014 server-side plumbing only (inbound service \u2192 zebra-state); drive_outbound_request returns PeerError::LocalOnlyRequest for them (connection.rs:1703)",
      "PeerCapability \u2014 Any vs V2; drives P2C filtering in peer_set/set.rs:922",
      "SharedConnection \u2014 per-connection shared state: quic handle, consecutive_timeouts, responses_received, active_bulk_streams SlotCounter, artifact_dir",
      "spawn_outbound_request / drive_outbound_request / send_wire_request \u2014 the whole client request pipeline: each internal request gets its own tokio task, its own bidi stream, and a REQUEST_TIMEOUT",
      "OutboundError \u2192 SharedPeerError mapping (connection.rs:1404-1467): Timeout counts toward 3-strike disconnect only if peer was fully silent; Refused reset leaves connection usable; protocol violations close connection with exact blame",
      "LoadTrackedClient::is_v2 + PeakEwma Load \u2014 the per-peer load signal P2C weighting already uses"
    ],
    "integration_points": [
      "Engine \u2192 network: the peer set is a tower Service<zebra_network::Request> returned by peer_set/initialize.rs init(); Request::BlockRange routes P2C over ready v2 peers only (peer_set/set.rs:1058 \u2192 :922-933)",
      "Client construction: peer/v2/service.rs:354 builds the same crate::peer::Client as legacy handshakes, so v2 peers are indistinguishable to callers except via ConnectionInfo.remote = RemoteHandshake::Init (load_tracked_client.rs:62)",
      "Client request path: Client.server_tx (mpsc channel(0)) \u2192 Connection::run loop (connection.rs:573) \u2192 dispatch_client_request:828 \u2192 spawn_outbound_request:1349 \u2014 per-request task means one Client supports many concurrent in-flight requests (peer must allow \u226532 bidi streams, constants.rs:123); tower load tracking counts them via CompleteOnResponse",
      "Server request path: quic.accept_bi (connection.rs:581) \u2192 serve_inbound_request_stream:1832 \u2192 serve_request:2078 \u2192 inbound service (zebrad/src/components/inbound.rs:607,614) \u2192 zebra-state ReadRequest::{SyncHashes,TreeRoots}",
      "get-object server: connection.rs:2506 reads {artifact_dir}/{lowercase-hex-sha256}; dir comes from config.cache_dir.artifact_dir_path (peer/v2/service.rs:135); NODE_SYNC_ARTIFACTS advertised at peer_set/initialize.rs:263 when dir exists",
      "BlockRange client-side verification (connection.rs:1652-1695): anchor hash \u2192 parent-chain \u2192 merkle root per block, count/max_bytes flood checks \u2014 the engine gets pre-hash-verified blocks for free from a trusted anchor",
      "Timeout/backpressure: whole outbound request bounded by constants::REQUEST_TIMEOUT (connection.rs:1360); consecutive fully-silent timeouts (3) disconnect peer (constants.rs:166, connection.rs:452-472); server refuses >2 concurrent get-block-range per peer via SlotCounter (connection.rs:2387-2390) and Refused surfaces to the caller as retryable PeerError::V2Protocol('peer refused BlockRange') (connection.rs:1449-1451)"
    ],
    "port_notes": [
      "(a) headers/hashes sync: CLIENT GAP \u2014 internal Request::SyncHashes is local-only by design; drive_outbound_request errors LocalOnlyRequest (connection.rs:1703). The engine needs either a new peer-routable internal variant (e.g. Request::RemoteSyncHashes with PeerCapability::V2, decoding via the already-written-but-dead HashesResponse::read, response.rs:321) or direct use of the wire layer; the wire encode/decode and the server side are complete and tested. FindHeaders/FindBlocks work over v2 today (bridged onto get-headers, connection.rs:1530-1540) for headers-first-to-checkpoint",
      "(b) parallel block-range fetch: Request::BlockRange is fully working end to end today (client connection.rs:1622, server :2378) \u2014 zebrad just has no caller yet. Per-peer weighting: the peer set only exposes P2C-over-load with the V2 capability filter; there is no public API to enumerate ready v2 peers, target a specific peer, or read per-peer load (query_load is private). The engine must either accept P2C (load-weighted 2-choice, likely adequate) or add a peer-set API (respect NO-MUTEX rule: watch channel or request variant). Work-unit sizing must respect server-side MAX_CONCURRENT_BULK_STREAMS=2 per peer and 64MiB/65,536-block caps; Refused \u2192 retry on another peer is the intended flow; truncated responses resume from last block's previous_block_hash",
      "(c) get-object: CLIENT GAP \u2014 wire Request::GetObject encode + full server exist, but there is NO internal Request/Response variant and no client decode path at all (the get-object response framing \u2014 RESULT byte + CompactSize total size + raw bytes \u2014 has a server encoder but no reader anywhere). Port must add: internal Request::Object{hash,offset,length} (PeerCapability::V2, ideally also filtered on peer NODE_SYNC_ARTIFACTS from remote_init.services), a response reader in drive_outbound_request, and a Response variant carrying (total_size, bytes). Pieces are capped at 32MiB (MAX_GET_OBJECT_LENGTH) \u2014 whole-bitmap spentness artifact must be chunked to \u226432MiB fetches and SHA-256-verified by the engine (transport does not verify object content)",
      "get-tree-roots client is likewise server-only: internal Request::TreeRoots is local-only; headers-first hint verification needs a peer-routable variant using the dead TreeRootsResponse::read (response.rs:366); server refuses when anchor not in best chain (Response::TreeRoots(None) \u2192 Refused, connection.rs:2497-2499)",
      "Reusable as-is: all wire structs/codecs (protocol/v2/request.rs, response.rs, record.rs), the whole outbound request pipeline (open stream, timeout, error/blame mapping, misbehavior scoring), server side of all four sync requests, artifact dir serving, NODE_SYNC_ARTIFACTS advertisement, BlockRange in-stream verification. Rewrite/new: engine-facing internal variants for SyncHashes/TreeRoots/Object, any per-peer scheduling beyond P2C, artifact download/verify/manage layer (writing fetched artifacts into cache_dir artifact_dir would also make the node serve them)",
      "Conflicts with current APIs: internal Request::SyncHashes{count:u32}/TreeRoots{count:u32} names are taken by the local-only server plumbing \u2014 a remote-fetch variant needs distinct names or a direction flag; Response::Blocks (shared with BlocksByHash) is how BlockRange answers, so the engine gets InventoryResponse-wrapped Arc<Block>s, not raw bytes; the checked-out branch is v2-stack (recent history also shows p2p-v2-quic commits merged in: atomic inbound slots, supervised endpoint tasks, silent-peer-only timeout counting)"
    ],
    "risks": [
      "HashesResponse::read/TreeRootsResponse::read are marked for 'Phase 5 v2 synchronization requester' \u2014 the plan already reserves this work, so coordinate with the P2P v2 plan phases (memory: project_p2p_v2_plan) to avoid diverging designs",
      "MIN_V2_PROTOCOL_VERSION is a placeholder (constants.rs:177 TODO) \u2014 version gating may change under the engine",
      "Per-peer weighting beyond P2C requires new peer-set surface; the NO-MUTEX rule constrains how per-peer load/choice state can be shared (watch/atomics/semaphore only)",
      "REQUEST_TIMEOUT bounds a whole BlockRange response; a full 64MiB range on a slow link could hit the timeout \u2014 engine work-unit sizing must consider constants::REQUEST_TIMEOUT, and a timed-out-but-partially-delivered range still counts toward the 3-strike disconnect only if the peer sent nothing at all",
      "No NODE_SYNC_ARTIFACTS-aware routing exists: a naive Request::Object over P2C would hit v2 peers without artifact stores and see Refused; the engine should filter on advertised services (remote_init.services) or tolerate refusals",
      "quinn stream-limit backpressure: >32 concurrent outbound requests to one peer will queue at open_bi().await inside the per-request task while REQUEST_TIMEOUT is already running"
    ]
  },
  {
    "component": "v2-stack state side: sync-metadata CF (28.1.0), DB upgrade framework, and commit/upgrade hook points for the combined format",
    "files": [
      {
        "path": "zebra-state/src/constants.rs",
        "role": "DB format version: DATABASE_FORMAT_VERSION=28, DATABASE_FORMAT_MINOR_VERSION bumped 0->1 at line ~53-56 with a doc comment describing 28.1.0 (sync_meta_by_height CF); patch version separate. Version bump is just this constant plus a registered DiskFormatUpgrade whose version() matches.",
        "loc": 5
      },
      {
        "path": "zebra-state/src/service/finalized_state/disk_format/chain.rs",
        "role": "SyncMetadata struct + BLOCK_SIZE_VALUE_UNIT (MAX_BLOCK_BYTES.div_ceil(255)) + IntoDisk/FromDisk (56-byte LE record; FromDisk asserts len>=56 and ignores trailing bytes, explicitly forward-compatible for appended fields). SyncMetadata::for_block(block, serialized_size) computes size, tx_count, note_count (sprout+sapling+orchard+ironwood commitments), sapling/orchard/ironwood tx counts, auth_data_root.",
        "loc": 105
      },
      {
        "path": "zebra-state/src/service/finalized_state/zebra_db/chain.rs",
        "role": "SYNC_META=\"sync_meta_by_height\" const, SyncMetaCf<'cf> = TypedColumnFamily<Height, SyncMetadata>, sync_meta_cf()/sync_metadata()/sync_metadata_map(range) readers; commit-time write at end of DiskWriteBatch::prepare_chain_value_pools_batch (line ~283-338): serializes block once for size (block.zcash_serialized_size(), shared with BlockInfo::new), then zs_insert(&finalized.height, &SyncMetadata::for_block(...)) at line ~334.",
        "loc": 41
      },
      {
        "path": "zebra-state/src/service/finalized_state/zebra_db/block.rs",
        "role": "prepare_block_batch calls prepare_chain_value_pools_batch at line 689 \u2014 runs for ALL heights including genesis (outside the !height.is_min() guard that skips transparent indexes for genesis). This is the finalized-commit choke point that sees the whole block.",
        "loc": 0
      },
      {
        "path": "zebra-state/src/service/finalized_state/disk_format/upgrade.rs",
        "role": "Upgrade framework: DiskFormatUpgrade trait (version/description/run(initial_tip_height, db, cancel_receiver)/validate/prepare/needs_migration, lines 42-90); format_upgrades() array at line ~92-115 filtered by upgrade.version() > older_disk_version; apply_format_upgrade (line 550) runs prepare->run->validate->mark_as_upgraded_to per upgrade, empty DB short-circuits to mark_as_upgraded_to (CFs themselves are created at DB open from STATE_COLUMN_FAMILIES_IN_CODE, not by upgrades). Cancellation = crossbeam bounded channel polled via try_recv; upgrade runs on a background thread (spawn_format_change line 329) so the node stays usable during backfill.",
        "loc": 900
      },
      {
        "path": "zebra-state/src/service/finalized_state/disk_format/upgrade/add_sync_metadata.rs",
        "role": "The 28.1.0 upgrade: version()=28.1.0; run() resumes from sync_meta_cf().zs_last_key_value()+1 (height-ordered writes make cancel/resume trivial), loops in BATCH_HEIGHTS=2_000-block DiskWriteBatches, per batch checks cancel via try_recv, reads db.block_and_size(height) and writes SyncMetadata::for_block; validate() checks metadata exists at finalized tip.",
        "loc": 96
      },
      {
        "path": "zebra-state/src/service/finalized_state.rs",
        "role": "STATE_COLUMN_FAMILIES_IN_CODE gets SYNC_META appended (line ~109-111); disk_format made pub(crate).",
        "loc": 6
      },
      {
        "path": "zebra-state/src/service/finalized_state/disk_format/tests/snapshots/column_family_names.snap",
        "role": "Snapshot updated with \"sync_meta_by_height\"; a second CF needs the same one-line addition here.",
        "loc": 1
      },
      {
        "path": "zebra-state/src/service/finalized_state/disk_format/tests/snapshots/empty_column_families@no_blocks.snap",
        "role": "Snapshot updated with \"sync_meta_by_height: no entries\"; second CF needs same.",
        "loc": 1
      },
      {
        "path": "zebra-state/src/request.rs",
        "role": "ReadRequest::SyncHashes{start_height, stride, count} and ReadRequest::TreeRoots{start_height, final_hash, count} (lines ~1129-1168), plus metric names sync_hashes/tree_roots.",
        "loc": 40
      },
      {
        "path": "zebra-state/src/response.rs",
        "role": "ReadResponse::SyncHashes(Vec<SyncHashEntry>) and ReadResponse::TreeRoots(Option<Vec<TreeRootsEntry>>) (types from zebra_chain::block::sync_metadata); both excluded from TryFrom<ReadResponse> for Response.",
        "loc": 14
      },
      {
        "path": "zebra-state/src/service.rs",
        "role": "ReadStateService dispatch: SyncHashes -> read::sync_hashes(state.latest_best_chain(), &state.db, ...), TreeRoots -> read::tree_roots(&state.db, ...) (line ~1439-1464).",
        "loc": 25
      },
      {
        "path": "zebra-state/src/service/read/block.rs",
        "role": "sync_hashes() (line 384): serves only finalized best chain at/below tip - MAX_BLOCK_REORG_HEIGHT; one range read of sync_meta CF (sync_metadata_map) covering all spans; per-entry span aggregate of size-value (u64::from(size).div_ceil(BLOCK_SIZE_VALUE_UNIT).clamp(1,255)), txs, notes; truncates to a prefix on backfill gap or missing hash. tree_roots() (line 476): refuses (None) unless db.height(final_hash)==start_height+count-1 anchors the best chain and every height has metadata + trees; per-pool roots zeroed before activation heights (Sapling/Nu5/Nu6_3).",
        "loc": 165
      },
      {
        "path": "zebra-chain/src/block/sync_metadata.rs",
        "role": "SyncHashEntry{hash, span_size, span_txs, span_notes} and TreeRootsEntry{sapling/orchard/ironwood_root, sapling/orchard/ironwood_txs, auth_data_root} \u2014 wire-facing entry types re-exported from zebra_chain::block.",
        "loc": 0
      },
      {
        "path": "zebra-network/src/protocol/internal/request.rs",
        "role": "Internal Request::BlockRange{final_hash,count,max_bytes} (peer-routed), Request::SyncHashes and Request::TreeRoots (local-inbound-only, never routed to a remote peer) at lines ~217-296.",
        "loc": 79
      },
      {
        "path": "zebra-network/src/protocol/internal/response.rs",
        "role": "Response::SyncHashes(Vec<SyncHashEntry>) (prefix-truncatable) and Response::TreeRoots(Option<Vec<TreeRootsEntry>>) (None = refused).",
        "loc": 23
      },
      {
        "path": "zebra-network/src/peer/connection.rs",
        "role": "Legacy v1 connection rejects BlockRange/SyncHashes/TreeRoots with PeerError::LocalOnlyRequest (line ~1074) instead of fabricating empty answers; ignores those Response variants when acting as responder (line ~1499).",
        "loc": 13
      },
      {
        "path": "zebra-network/src/peer/v2/connection.rs",
        "role": "v2 QUIC serving path: WireRequest::GetHashes -> call_inbound_service(Request::SyncHashes) -> HashesResponse(entries).encode (line ~2453-2476, wire count bounded by MAX_GET_HASHES_COUNT=50_000); WireRequest::GetTreeRoots -> Request::TreeRoots, Response::TreeRoots(Some(_)) required else ServeError::Refused (line ~2477-2505, MAX_GET_TREE_ROOTS_COUNT=4_000); outbound side rejects local-only requests at line 1703. Also WireRequest::GetObject serves artifacts from shared.artifact_dir by hex hash with RESULT_NOT_FOUND fallback (line ~2507+) \u2014 the hook for SwiftSync bitmap artifact distribution already exists.",
        "loc": 0
      },
      {
        "path": "zebrad/src/components/inbound.rs",
        "role": "InboundSetupData/Setup gain a zs::ReadStateService handle; zn::Request::SyncHashes/TreeRoots map 1:1 to zs::ReadRequest::SyncHashes/TreeRoots via read_state.clone().oneshot (line ~604-620); zebrad/src/commands/start.rs passes read_state into setup.",
        "loc": 34
      }
    ],
    "key_types": [
      "SyncMetadata (disk_format/chain.rs) \u2014 per-block record: size u32, tx_count u32, note_count u32, sapling/orchard/ironwood_tx_count u32, auth_data_root [u8;32]; 56-byte LE encoding, FromDisk tolerates trailing bytes (append-friendly)",
      "BLOCK_SIZE_VALUE_UNIT \u2014 MAX_BLOCK_BYTES.div_ceil(255); raw size stored, 1-byte quantization applied at serve time",
      "SYNC_META / SyncMetaCf<'cf> = TypedColumnFamily<'cf, Height, SyncMetadata> \u2014 CF name \"sync_meta_by_height\", key = Height",
      "SyncMetadata::for_block(&Block, serialized_size) \u2014 computes the record; used identically at commit and in the backfill upgrade",
      "DiskFormatUpgrade trait \u2014 version/description/prepare/run/validate/needs_migration; run gets (initial_tip_height, &ZebraDb, &Receiver<CancelFormatChange>)",
      "add_sync_metadata::Upgrade \u2014 28.1.0 backfill: resume from zs_last_key_value, 2000-block batches, cancel poll per batch, validate = tip entry exists",
      "ReadRequest::SyncHashes / ReadResponse::SyncHashes(Vec<SyncHashEntry>) \u2014 strided best-chain hashes + span aggregates, prefix truncation only",
      "ReadRequest::TreeRoots / ReadResponse::TreeRoots(Option<Vec<TreeRootsEntry>>) \u2014 anchored by final_hash; None = must refuse, never partial",
      "read::sync_hashes / read::tree_roots (read/block.rs:384/476) \u2014 serving fns; sync_hashes caps at tip - MAX_BLOCK_REORG_HEIGHT so answers are reorg-safe",
      "zebra_chain::block::{SyncHashEntry, TreeRootsEntry} \u2014 shared wire/state entry types",
      "DiskWriteBatch::prepare_chain_value_pools_batch \u2014 the commit-path fn that serializes the block for size and writes BlockInfo + SyncMetadata",
      "check_cancelled \u2014 NOT shared: 2fc405d60 deliberately reverted the shared helper back into add_ironwood_tree.rs as a private fn; add_sync_metadata inlines the try_recv check"
    ],
    "integration_points": [
      "Wire -> state chain for get-hashes: zebra-network/src/peer/v2/connection.rs:2453 (WireRequest::GetHashes) -> internal Request::SyncHashes -> zebrad/src/components/inbound.rs:~604 (zn::Request::SyncHashes -> zs::ReadRequest::SyncHashes via read_state.oneshot) -> zebra-state/src/service.rs:~1439 -> read::sync_hashes (zebra-state/src/service/read/block.rs:384)",
      "Wire -> state chain for get-tree-roots: zebra-network/src/peer/v2/connection.rs:2477 -> Request::TreeRoots -> inbound.rs -> ReadRequest::TreeRoots -> read::tree_roots (read/block.rs:476)",
      "Commit-time write: zebra-state/src/service/finalized_state/zebra_db/block.rs:689 (prepare_block_batch) -> prepare_chain_value_pools_batch (zebra_db/chain.rs:283) -> sync_meta_cf write at chain.rs:334; runs for every finalized block including genesis",
      "Upgrade registration: disk_format/upgrade.rs:~114 format_upgrades array entry Box::new(add_sync_metadata::Upgrade) as 7th upgrade; executed by apply_format_upgrade (upgrade.rs:550) then mark_as_upgraded_to writes the on-disk version",
      "CF creation: finalized_state.rs STATE_COLUMN_FAMILIES_IN_CODE (~line 109) \u2014 CFs created at DB open, upgrades only backfill data",
      "Artifact serving hook for SwiftSync: WireRequest::GetObject + shared.artifact_dir in zebra-network/src/peer/v2/connection.rs:~2507 (refused when no artifact dir; NODE_SYNC_ARTIFACTS service bit gated)",
      "Inbound wiring: zebrad/src/commands/start.rs passes ReadStateService into InboundSetupData (1 line)"
    ],
    "port_notes": [
      "Combined 28.1.0 approach: the 28.1.0 bump is not yet shipped anywhere, so adding the known-hash chunk CF and the cumulative transparent-output count to the SAME bump means (a) new CF name const + TypedColumnFamily alias in zebra_db (new file or chain.rs), (b) append the CF to STATE_COLUMN_FAMILIES_IN_CODE, (c) extend add_sync_metadata::Upgrade::run to also backfill the new data (or rename the module, e.g. add_sync_metadata -> a combined 28.1.0 upgrade), (d) update the two CF snapshot files, (e) extend validate() to check the new CF at tip. No second Version entry is needed \u2014 one DiskFormatUpgrade per version; format_upgrades() filters by version so it runs once.",
      "Resume correctness across the combined upgrade: current resume key is sync_meta CF's last key. With multiple CFs written in the same batch per height, resume from the MIN of the last keys across the CFs (or keep writing all per-height records in one DiskWriteBatch so the frontiers can never diverge \u2014 batches are atomic, so min == all if grouped per batch). Dev DBs that already ran the pure sync-meta 28.1.0 will have sync_meta full but chunk CF empty \u2014 resume-from-min handles that; resume-from-sync-meta-last would silently skip the backfill.",
      "Cumulative transparent-output count placement options: (1) append a u64 to SyncMetadata (FromDisk already tolerates >56-byte records by design, and 28.1.0 is unreleased so a clean 64-byte record is fine) or (2) a separate column/extension of BlockInfo. Appending to SyncMetadata is cheapest: same CF, same backfill loop, same commit write.",
      "Where the running total is visible at commit: prepare_chain_value_pools_batch (zebra_db/chain.rs:283) sees finalized.block and finalized.height and already reads prior state (chain value pool) \u2014 read the previous height's SyncMetadata (db.sync_metadata(prev_height)) to get the prior cumulative, add sum of tx.outputs().len() over block.transactions. Finalized commits are strictly sequential on the write path, so height-1 is always readable. Genesis: count its outputs but note the consensus rule that genesis outputs are unspendable (the value-pool code skips genesis transparent indexes; decide whether cumulative count includes genesis's 1 output \u2014 must match the SwiftSync spec's output ordinal definition).",
      "Where the running total is visible during upgrade: add_sync_metadata::Upgrade::run's height loop (upgrade/add_sync_metadata.rs:48-73) walks 0..=tip in order with block_and_size(height) \u2014 thread a mut cumulative through the loop, seeded on resume from the last written record's cumulative field (the resume read at lines 42-46 already fetches the last (height, SyncMetadata) pair, so the seed is free).",
      "SyncHashes serving is DB-only (read::sync_hashes ignores the non-finalized chain except for tip height; entries capped at tip - MAX_BLOCK_REORG_HEIGHT), so new columns only need finalized-state writes \u2014 no non-finalized Chain struct changes required for serving. The ibd-engine port's known-hash chunk CF likewise only needs the finalized path.",
      "Refusal semantics to preserve: tree_roots returns Option (None = refuse at wire level with ServeError::Refused), sync_hashes truncates to a prefix; a backfill gap truncates rather than under-reports. Mirror this for chunk/artifact serving during the upgrade window.",
      "No mutexes constraint: state side already conforms (crossbeam channel for cancel, read-only ReadStateService handle cloned into inbound).",
      "Conflict watch: v2-stack history was rewritten recently (HEAD 3f8b9d985, but zebra-state commits 04cbce21c/2fc405d60 still in origin/main..v2-stack); 2fc405d60 reverted the shared check_cancelled helper out of upgrade.rs \u2014 do not reintroduce it when extending the upgrade; keep the inline try_recv pattern or a module-private helper.",
      "Reusable as-is for the port: DiskFormatUpgrade framework, TypedColumnFamily machinery, SyncMetadata encode/decode, the batched-backfill loop shape, GetObject artifact serving, and the inbound ReadStateService plumbing. Rewrite needed: add_sync_metadata upgrade body (extend), SyncMetadata record (append field), constants.rs doc comment for 28.1.0 (describe all three additions), snapshots."
    ],
    "risks": [
      "Dev/test DBs already marked 28.1.0 by the current upgrade will never re-run the extended upgrade (format_upgrades filters version > disk version); since 28.1.0 is unreleased this only affects local DBs \u2014 either delete them or bump to 28.1.x/28.2.0 locally if any exist (user's own machines only).",
      "FromDisk for SyncMetadata asserts len>=56 and reads a fixed layout \u2014 appending a field changes the record length; any code assuming exactly 56 bytes (Vec::with_capacity(56) in IntoDisk) must be updated together.",
      "tree_roots does one sapling+orchard+ironwood tree read per height (up to 4000 heights per wire request) \u2014 adding more per-height reads to this path multiplies request cost; the cumulative-count column should be served from the same SyncMetadata record read that already happens.",
      "validate() only checks the tip entry exists; a hole in the middle of a CF (possible only via bugs, since writes are height-ordered batches) would pass validation \u2014 the combined upgrade should keep the write-in-height-order invariant for every new column.",
      "sync_hashes uses db.hash(height) for the entry hash but the metadata map read is a separate CF read; during a concurrent reorg-window this is safe only because served heights are >= MAX_BLOCK_REORG_HEIGHT below tip \u2014 keep that bound for any new serving path (known-hash chunks)."
    ]
  },
  {
    "component": "Known-hash IBD engine (branch ibd-engine, closed PR #10725): pinned-hash-list initial sync replacing the checkpoint verifier, plus generic-engine tip-following sync, snapshot-consume (assumeUTXO) mode, two-tier state write pipeline, and the known_hash_chunk column family",
    "files": [
      {
        "path": "ibd-engine:zebrad/src/components/ibd.rs",
        "loc": 677,
        "role": "Supervisor: precondition checks (config flag, bundled list, tip vs list max), restart loop with IBD_RESTART_DELAY/IBD_MAX_RESTARTS_WITHOUT_PROGRESS degradation policy, IbdOutcome, commit_genesis_if_missing, spawn_engine_then_tip_sync (engine-then-syncer handoff)"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/engine.rs",
        "loc": 2473,
        "role": "The Engine: VecDeque ring window of Slots keyed by height, FuturesUnordered of per-block staged futures (fetch/hedge/commit/tree), byte budgets, gap hedging, stall detection, commit-failure detector, disk-tier eviction; generic over HashSource and CommitStage"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/fetch.rs",
        "loc": 535,
        "role": "Weighted batched fetch: FetchRequest (height+hash+size_hint), FetchBatcher inner service under tower-batch-control Batch; one zn::Request::BlocksByHash per flush, per-item responders, NotFound vs Transport failure classification"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/cache.rs",
        "loc": 584,
        "role": "BlockCache disk overflow tier under <state cache_dir>/ibd-block-cache/: per-block .bin files with sidecar (magic zebra-ibd-cache-v1), hash-named entries, verify_entry re-check on read-back, lazy batched eviction"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/convert.rs",
        "loc": 634,
        "role": "Stage 2 for known-hash mode: convert() (pure CPU: coinbase-height, prev-link, merkle-root checks + CheckpointVerifiedBlock::with_hash) run on rayon via zebra_consensus::spawn_fifo; CommitStage trait; VerifyAndCommit service committing via zs::Request::CommitCheckpointVerifiedBlock; BlockPayload (Arc<Block> | untrusted Raw from disk); IbdBlock; SuppliedTrees"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/semantic.rs",
        "loc": 215,
        "role": "SemanticCommit: full-validation CommitStage wrapping the semantic block verifier (zebra_consensus::Request), used by tip-following sync through the same engine"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/discovery.rs",
        "loc": 351,
        "role": "DiscoverySource + DiscoveryFeed: growing/finalizable HashSource fed by the syncer's crawl (obtain/extend_tips), DISCOVERY_LOW_WATER_MARK demand signal (wants_more_hashes), wait_for_growth"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/tree.rs",
        "loc": 316,
        "role": "TreeLookahead: bounded buffer (TREE_BUFFER_MAX_BYTES 64MB, TREE_LOOKAHEAD_MAX 16384) of pre-fetched verified note commitment trees keyed by height, in-flight tree-fetch set; snapshot-consume only"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/consume.rs",
        "loc": 600,
        "role": "CfHashSource<ZS>: HashSource backed by the known_hash_chunk CF + LocalSnapshotSource artifact dir; verifies chunk bytes against pinned SHA-256 (verify_chunk_bytes), persists verified chunks via Request::WriteKnownHashChunk, serves tree_updates_in/tree_root from parsed v2 chunks"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/consume/local.rs",
        "loc": 370,
        "role": "LocalSnapshotSource: reads chunk/tree/survivor artifacts from the installer-downloaded (or emit-snapshot --emit-files) directory"
      },
      {
        "path": "ibd-engine:zebrad/src/components/ibd/embedded_assets.rs",
        "loc": 481,
        "role": "open_or_materialize: opt-in embedded chunk assets (zebra-known-hashes data crates) written to the platform data dir on first run, then KnownHashList::open"
      },
      {
        "path": "ibd-engine:zebra-chain/src/parameters/known_hashes.rs",
        "loc": 727,
        "role": "KnownHashListSpec (max_height, chunk_blocks=HASHES_PER_CHUNK=150_000, pinned per-chunk SHA-256 hex constants, file_prefix, unspent_outputs_hash/address_balances_hash Options); MAINNET (23 chunks, 0..=3,373,206) / TESTNET (28 chunks, 0..=4,057,200) constants; KnownHashList windowed loader (MAX_RESIDENT_CHUNKS=2, LRU, open() verifies EVERY chunk's SHA-256 once then drops bytes; on-demand chunk residency; search-dir resolution)"
      },
      {
        "path": "ibd-engine:zebra-chain/src/parameters/known_hashes/chunk_v2.rs",
        "loc": 655,
        "role": "ZKH2 v2 chunk framing: magic+version header, block hashes, optional per-block size hints (FLAG_HAS_HINTS), sparse sapling/orchard TreeRoot sections (FLAG_HAS_TREE_ROOTS); encode(), ParsedChunk zero-copy parser, is_v2 sniffing (v1 bare n\u00d732/n\u00d733 still accepted by the loader)"
      },
      {
        "path": "ibd-engine:zebra-state/src/service/finalized_state/zebra_db/known_hash.rs",
        "role": "known_hash_chunk column family (KNOWN_HASH_CHUNK, index u32 -> raw chunk_v2 bytes): read/write handles, part of DB format 28.1.0 on that branch"
      },
      {
        "path": "ibd-engine:zebra-state/src/constants.rs",
        "role": "DATABASE_FORMAT 28.1.0 on this branch = known_hash_chunk CF (NOTE: v2-stack already uses 28.1.0 for the sync_meta_by_height CF \u2014 the combined format must merge both plus the per-height cumulative transparent-output count)"
      },
      {
        "path": "ibd-engine:zebra-state/src/request.rs",
        "loc": 207,
        "role": "New requests: Request::KnownHashChunk(u32) (line 1202, read-or-generate), Request::WriteKnownHashChunk{index,bytes} (line 1216, side-index write bypassing the block write task), ReadRequest::KnownHashChunk (line 1663), ReadRequest::IsTransparentOutputSpent; CommitCheckpointVerifiedBlock carries optional supplied trees"
      },
      {
        "path": "ibd-engine:zebra-state/src/service/write/worker.rs",
        "loc": 949,
        "role": "Thread 1 of the two-tier commit: WriteBlockWorker reads WriteMessages from one channel, commits to in-memory NFS, acks checkpoint blocks as soon as in-memory, hands durable-bound blocks to the disk writer over a bounded channel; documented per-path error policy table; prune_durable_blocks"
      },
      {
        "path": "ibd-engine:zebra-state/src/service/write/disk_writer.rs",
        "loc": 244,
        "role": "Thread 2: sole caller of FinalizedState::commit_finalized_direct, strict parent-linked order; FinalizedWritePhase guard pausing RocksDB auto-compaction (and optionally WAL) during bulk checkpoint writes; DiskRequest::EndBulk transition"
      },
      {
        "path": "ibd-engine:zebra-state/src/service/write/rpc_indexer.rs",
        "loc": 169,
        "role": "Thread 3 (only with Config::separate_rpc_index_db): trails the durable tip via one AtomicU32, writes RPC-only transparent indexes into a separate DB with its own rpc_index_tip marker; atomic per-block batches"
      },
      {
        "path": "ibd-engine:zebra-state/src/snapshot_consume.rs",
        "loc": 735,
        "role": "SnapshotConsumeConfig ([state] snapshot_consume: survivor_set_path, h_max, elide_utxo_bytes) and SurvivorSet: sorted 8-byte OutputLocation records, loaded with SHA-256 verification against the pinned constant (MAX 8GB); is_survivor_bytes gate for eliding non-survivor UTXO/address-index writes; crash-safety argument documented in module header"
      },
      {
        "path": "ibd-engine:zebra-state/src/service/read/snapshot.rs",
        "loc": 595,
        "role": "Deterministic artifact generation from finalized state: known_hash_chunk_bytes (chunk_v2 re-encoding of a 150k span incl. tree-root updates), note_commitment_tree_bytes; the emit side of content addressing"
      },
      {
        "path": "ibd-engine:zebra-state/src/service/check/utxo.rs",
        "loc": 109,
        "role": "UTXO validation changes supporting elision (spend resolution no longer reads spent value from utxo_by_out_loc); plus ffb09bd4c O(N^2) fix context"
      },
      {
        "path": "ibd-engine:zebra-consensus/src/checkpoints.rs",
        "loc": 138,
        "role": "Replaces the whole checkpoint verifier + router (checkpoint.rs 1177 LOC and router.rs 450 LOC are DELETED on this branch): max_checkpoint_height() and CheckpointGateLayer/CheckpointGate rejecting semantic commits at or below the mandatory checkpoint height"
      },
      {
        "path": "ibd-engine:zebrad/src/components/sync.rs",
        "loc": 1002,
        "role": "Tip-following syncer rewritten to drive Engine::new_semantic over a DiscoverySource per sync_cycle (crawl feeds hashes concurrently with fetch/verify/commit); sync::Config gains known_hash_sync (default true), known_hash_lookahead_bytes (256MB), known_hash_gap_hedge_secs (5), known_hash_tree_lookahead, known_hash_list_dir, known_hash_local_source_dir, snapshot_consume_sync; sync/downloads.rs (629 LOC) deleted"
      },
      {
        "path": "ibd-engine:zebrad/src/commands/start.rs",
        "role": "Wiring: max_finalizable_height = max(max_checkpoint_height, list max) into zebra_state::init (line ~355); ibd_cache_dir (tempdir when ephemeral, line ~453); ChainSync::new takes peer_set_status + cache_dir; ibd::commit_genesis_if_missing then IbdEngine::new (line 699) and spawn_engine_then_tip_sync (line 708) as the syncer task"
      },
      {
        "path": "ibd-engine:zebrad/src/commands/emit_snapshot.rs",
        "loc": 1120,
        "role": "emit-snapshot command: emits v2 chunks, note commitment trees, unspent-output (survivor) and address-balance sets from a synced state; constants-updater editor (editor.rs, 453 LOC) rewrites the pinned hex constants in known_hashes.rs at release"
      },
      {
        "path": "ibd-engine:zebra-network/src/peer_set/set.rs",
        "loc": 534,
        "role": "PeerSetStatus { ready_peers } watch published by the peer set (line 181), used by ibd_max_concurrent_batches sizing and stall diagnostics; also FindBlocks/FindHeaders stall tracking"
      },
      {
        "path": "ibd-engine:tower-fair-buffer/src/lib.rs",
        "loc": 1173,
        "role": "Fair replacement for tower::Buffer around the peer set (whole crate, 8 modules): per-tag fair queueing so the IBD engine's bulk fetches don't starve inbound/mempool requesters; zebra-network adopts it in isolated/handshake/initialize paths"
      },
      {
        "path": "ibd-engine:zebra-known-hashes/src/lib.rs",
        "loc": 26,
        "role": "Umbrella crate over 51 per-chunk publish=false data crates (zebra-known-hashes/chunks/*, ~4.95MB .bin each) + gen-chunk-crates.py; feeds embedded_assets.rs"
      },
      {
        "path": "ibd-engine:zebra-utils/src/bin/known-hashes/main.rs",
        "loc": 184,
        "role": "known-hashes tool (args/emit/source/sweep): builds and sweeps chunk assets from a node RPC"
      }
    ],
    "key_types": [
      "Engine<ZN, C, L> \u2014 the generic engine: ring window (VecDeque<Slot>, base = lowest uncommitted height) + one FuturesUnordered<BlockFut> of staged futures; generic over network service ZN, CommitStage C, HashSource L",
      "Slot \u2014 per-height state machine: Unrequested{attempts,not_founds,backoff} -> InFlight{since,hedged,abort} -> Fetched{block,bytes,committing,source} | Cached{bytes,promoted} (disk tier) -> Committed; popped from front as the contiguous committed prefix advances",
      "HashSource (trait) \u2014 per-height hash/size_hint provider with release_below, ensure_covers (async chunk priming), tree_updates_in/tree_root (snapshot mode), extend/invalidate_above/is_final/wait_for_growth (discovery growth+reorg seam)",
      "KnownHashList \u2014 windowed on-disk chunk loader: verifies all pinned SHA-256s at open, then keeps at most 2 chunks resident, LRU, loading on demand",
      "KnownHashListSpec \u2014 compile-time trust root: max_height, 150k-block chunks, pinned per-chunk SHA-256 hex, optional unspent_outputs_hash/address_balances_hash",
      "chunk_v2::ParsedChunk / TreeRoot \u2014 ZKH2 framing: hashes + optional size hints + sparse per-pool tree-root records; is_v2() distinguishes from legacy bare v1",
      "CommitStage (trait) \u2014 Service<IbdBlock, Response=block::Hash, Error=VerifyAndCommitError> + Clone + Send; blanket impl; the verify-strategy seam",
      "VerifyAndCommit<ZS> \u2014 known-hash stage 2: rayon spawn_fifo convert() (coinbase height, prev-link, merkle root) then zs::Request::CommitCheckpointVerifiedBlock; future resolves only after DB accept",
      "SemanticCommit<ZV> \u2014 full-validation stage 2 over the semantic block verifier; same engine, used by tip-following sync",
      "IbdBlock / BlockPayload / SuppliedTrees \u2014 stage-2 request: height, pinned expected + prev_expected hashes, Block-or-untrusted-Raw payload, source peer, optional pre-fetched verified trees",
      "FetchRequest / FetchBatcher<ZN> / BatchedFetch<ZN> = Batch<FetchBatcher,FetchRequest> \u2014 weighted batching: weight = size_hint x SIZE_HINT_UNIT (7844B) floored so 16 items always flush; one BlocksByHash per flush; NotFound vs Transport classification",
      "BlockCache / CachedBlock \u2014 disk overflow tier: hash-named entry files + sidecar, verify_entry hash re-check on promotion, lazy eviction every IBD_CACHE_EVICT_INTERVAL=1024 committed heights",
      "IbdEngine<ZN,ZS,ZSTip> (supervisor) + IbdOutcome{Completed,Declined(DeclineReason),Degraded} \u2014 restart loop re-deriving base from state tip; degrade to syncer only above the mandatory checkpoint",
      "EngineError \u2014 retryability split: ListDiagnostic/DeterministicCommitFailure/ArtifactDiagnostic/Shutdown fatal; List/Internal/InvalidDiscoveredBlock retryable",
      "DiscoverySource / DiscoveryFeed \u2014 growing HashSource fed by the syncer crawl; wants_more_hashes() below DISCOVERY_LOW_WATER_MARK=200 remaining",
      "TreeLookahead / TreeFetch / HeightTrees \u2014 bounded pre-fetch buffer of verified note commitment trees (snapshot-consume mode)",
      "CfHashSource<ZS> \u2014 HashSource over the known_hash_chunk CF + local artifact dir; verify-then-persist-then-cache per chunk",
      "SurvivorSet / SnapshotConsumeConfig / SnapshotConsumeState \u2014 sorted 8-byte output-location set, SHA-256 verified; gates non-survivor UTXO/address-index elision in the finalized write path",
      "WriteBlockWorker (Thread 1) / disk_writer (Thread 2, FinalizedWritePhase compaction guard) / rpc_indexer (Thread 3, AtomicU32-coupled trailing index) \u2014 the two-tier (memory-ack then disk) commit pipeline",
      "CheckpointGateLayer/CheckpointGate \u2014 rejects semantic commits at or below mandatory checkpoint height; all that remains of the checkpoint verifier",
      "PeerSetStatus{ready_peers} \u2014 watch from the peer set; ibd_max_concurrent_batches(ready) sizes fetch concurrency (cap IBD_MAX_CONCURRENT_BATCHES=96)",
      "Key constants: IBD_BATCH_MAX_BLOCKS=16 (GETDATA serving limit), IBD_BATCH_MAX_WEIGHT, IBD_WINDOW_MAX_BLOCKS=16384, IBD_SPAN_MAX=2M, IBD_COMMIT_PIPELINE_BLOCKS=1024 / _BYTES=64MB, IBD_FRONTIER_CRITICAL_SPAN=64, IBD_GAP_HEDGE_AFTER=5s, retry backoff 500ms..30s, COMMIT_FAILURE_ATTEMPT_LIMIT=3, default lookahead 256MB"
    ],
    "integration_points": [
      "Peer set (fetch): the ONLY network request the engine issues is zn::Request::BlocksByHash \u2014 batched in fetch.rs fetch_batch (~line 340) and single-hash gap hedges from engine.rs issue_fetch (~line 1433); failure classification relies on InventoryResponse Available/Missing and never writes the inventory registry",
      "Peer set (status): watch::Receiver<PeerSetStatus> published from zebra-network/src/peer_set/set.rs:181 and returned by zebra_network::init; consumed at engine.rs:188 ibd_max_concurrent_batches and by stall diagnostics; ChainSync::new and IbdEngine::new both take it (start.rs ~463, ~699)",
      "State (commit): VerifyAndCommit -> zs::Request::CommitCheckpointVerifiedBlock (convert.rs ~line 555); ack returns after in-memory commit (Thread 1), disk write trails (Thread 2); CommitBlockError::WriteTaskExited detection at engine.rs:308 drives shutdown-vs-refetch",
      "State (chunk CF): Request::KnownHashChunk (request.rs:1202) read-or-generate handled in service.rs:1879 via read::known_hash_chunk_bytes; Request::WriteKnownHashChunk (request.rs:1216) handled at service.rs:1323-1335 via spawn_blocking db.write_known_hash_chunk \u2014 side-index write bypassing block write ordering; CF declared in zebra_db/known_hash.rs:31",
      "State (init): zebra_state::init now takes max_finalizable_height \u2014 start.rs ~355 raises it to the known-hash list max so the finalized path covers the whole pinned list, not just spaced checkpoints",
      "Consensus: checkpoint verifier and router deleted; CheckpointGateLayer wraps the semantic verifier (checkpoints.rs); zebra_consensus re-exports merkle_root_validity and spawn_fifo used by convert.rs:35; max_checkpoint_height moved to zebra_consensus::checkpoints",
      "zebrad startup: start.rs:699 IbdEngine::new(config.sync, network, ibd_peer_set, state, latest_chain_tip, peer_set_status, &ibd_cache_dir); start.rs:708 ibd::spawn_engine_then_tip_sync(ibd_engine, syncer.sync()) \u2014 engine task runs to IbdOutcome, then syncer future starts; genesis pre-committed by ibd::commit_genesis_if_missing (start.rs:695)",
      "Tip-following sync: sync.rs sync_cycle (~line 635) builds DiscoverySource::new(base=tip+1, anchor=tip_hash), Engine::new_semantic (~line 682), runs engine.run() concurrently with the extend_tips crawl feeding DiscoveryFeed; commit_pipeline_blocks = full_verify_concurrency_limit",
      "Config: [sync] known_hash_sync/known_hash_lookahead_bytes/known_hash_gap_hedge_secs/known_hash_tree_lookahead/known_hash_list_dir/known_hash_local_source_dir/snapshot_consume_sync (sync.rs:264-402); [state] separate_rpc_index_db, snapshot_consume (SnapshotConsumeConfig), disable_wal_during_ibd, checkpoint_sync_retained_blocks, checkpoint_sync_pipeline_capacity (zebra-state/src/config.rs)",
      "Inbound: GETDATA_MAX_BLOCK_COUNT shared constant (engine.rs:68); inbound serving of chunk requests was dropped in 5ebe1e3de in favor of installer distribution \u2014 artifact intake is LocalSnapshotSource only on this branch",
      "tower-fair-buffer: replaces tower::Buffer for the peer set inside zebra-network (lib.rs/initialize.rs/isolated.rs), so engine bulk traffic and inbound/mempool requests get fair queueing"
    ],
    "port_notes": [
      "DB format collision: ibd-engine's 28.1.0 = known_hash_chunk CF, but v2-stack's 28.1.0 already = sync_meta_by_height CF (v2 get-hashes/get-tree-roots). Per program goal both merge into one combined 28.1.0 together with the per-height cumulative transparent-output count \u2014 the constants.rs version-history entries and the empty_column_families/column_family_names snapshots must be rewritten as one combined change, not cherry-picked",
      "Fetch path must be re-targeted to the v2 transport: zn::Request::BlocksByHash still exists on v2-stack as an internal request, so FetchBatcher can port nearly as-is, but batch sizing constants (IBD_BATCH_MAX_BLOCKS=16 from legacy GETDATA serving limits) and the notfound/Available classification should be revisited against v2 QUIC request semantics (per-request streams, different serving limits, and zips#1346 sync requests like get-hashes may replace the crawl)",
      "PeerSetStatus watch does not exist on v2-stack \u2014 port it (or an equivalent ready-peer count from the v2 peer set/peer book actor) before the engine can size concurrency; note the no-mutex rule: it is already watch-based, keep it that way",
      "v2-stack still has the legacy checkpoint verifier (zebra-consensus/src/checkpoint.rs 46K, router.rs) and legacy syncer (sync/downloads.rs): the port includes the checkpoint.rs/router.rs deletion -> checkpoints.rs CheckpointGateLayer rewrite, and the sync.rs rewrite that drives Engine::new_semantic \u2014 a large, invasive slice; decide whether to port it whole or stage engine-first with the legacy syncer retained",
      "v2-stack has only zebra-state/src/service/write.rs \u2014 the whole two-tier write pipeline (worker.rs/disk_writer.rs/rpc_indexer.rs, ~1,360 LOC + write.rs rework + finalized_state.rs changes, zebra_state::init max_finalizable_height signature change) is a prerequisite of the engine's commit-ack model and must be ported first or alongside",
      "Reusable nearly as-is (self-contained, no transport coupling): engine.rs ring/backpressure core, Slot machine, HashSource + CommitStage seams, cache.rs, convert.rs, tree.rs, discovery.rs, known_hashes.rs + chunk_v2.rs, embedded_assets.rs + zebra-known-hashes crates, snapshot_consume.rs, read/snapshot.rs, emit_snapshot command, consensus checkpoints.rs",
      "SwiftSync integration points (per program goal): headers-first hint verification and the whole-bitmap spentness artifact slot naturally into the existing seams \u2014 SurvivorSet is the closest analog to the spentness bitmap (replace its sorted-record set + SHA-256 with the whole-bitmap artifact + salted keyed-BLAKE2b-256 multiset aggregates); KnownHashListSpec's unspent_outputs_hash/address_balances_hash Options are the pinning pattern to extend; SuppliedTrees/tree lookahead shows how per-height hint payloads thread through CommitCheckpointVerifiedBlock into the write worker",
      "Chunk intake on ibd-engine is local-artifact-dir only (P2P snapshot distribution was designed then dropped, commit 5ebe1e3de); the v2 port's zips#1346 sync protocol (get-tree-roots etc.) likely reinstates P2P distribution \u2014 CfHashSource's verify-then-persist(WriteKnownHashChunk)-then-cache flow is the right seam to feed from a v2 network fetch instead of LocalSnapshotSource",
      "Ironwood: v2-stack has ironwood_* CFs and IronwoodTree/IronwoodSubtrees read requests; chunk_v2's tree-root sections and SuppliedTrees are sapling/orchard-only \u2014 the port should keep them sapling/orchard-only for the pre-H_max range but check TreeRoot section flags leave room (chunk_v2 has a reserved tail and flag bits available)",
      "tower-fair-buffer adoption touches zebra-network initialize/isolated paths that have materially diverged on v2-stack (QUIC endpoints, supervised tasks) \u2014 re-derive that integration rather than diffing it across; verify it is compatible with the v2 peer set's readiness model",
      "Mutex audit needed during port: the branch predates the no-mutex rule (memory notes p2p-v2 PR 1 mutexes needed rework); scan ported state-write and engine code for std/tokio Mutex before committing",
      "Upstream drift: ibd-engine's base b36a3ed59 predates upstream changes now on v2-stack (e.g. #11218 peer book reconciliation, NU6.3/28.0.0 format); expect conflicts in request.rs/response.rs variant lists, service.rs dispatch, and sync.rs \u2014 port by re-applying intent, not raw diffs"
    ],
    "risks": [
      "The port spans three of the largest subsystems at once (consensus verifier removal, state write pipeline, syncer rewrite) \u2014 hard to stage as reviewable commits without a deliberate seam-first ordering (write pipeline -> engine+supervisor -> checkpoint removal -> syncer rewrite)",
      "Mainnet/Testnet pinned chunk_hashes constants and bundled .bin assets are stale relative to today's chain tip; a port must re-run the known-hashes sweep/emit tooling and possibly re-emit v2 chunks, or ship with the old max_height (3,373,206)",
      "The engine's commit-ack semantics (checkpoint block acked when in-memory, disk trails) change crash-recovery behavior; the \u00a74.6 deterministic-commit-failure and reset-recovery logic is subtle and must be re-verified against the v2-stack state code, not assumed correct after merge",
      "batch weight/limit constants encode legacy transport serving behavior; keeping them unexamined on QUIC could either underutilize streams or trip v2 per-peer limits",
      "unspent_outputs_hash/address_balances_hash are still None (never pinned) \u2014 snapshot-consume mode was never runnable end-to-end from released constants; SwiftSync replaces this mechanism, so avoid porting dead snapshot-consume surface the new design supersedes",
      "The two-tier pipeline's disk thread was measured CPU-bound (~242 blk/s ceiling per memory notes); porting it unchanged carries that bottleneck into the v2 program"
    ]
  }
]
```
