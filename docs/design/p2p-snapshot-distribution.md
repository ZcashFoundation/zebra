# P2P Distribution of Known-Hash Chunks, Note Commitment Trees, and the Unspent-Output Set

Status: **IMPLEMENTED (snapshot-consume path dormant pending v2 asset
re-emission).** Base: branch `ibd-engine`. See §8 for what landed, the remaining
work, and how to manually test.

## 0. The architecture decision

The known-hash chunks, the note commitment trees, and the set of unspent
transparent output locations at the max checkpoint height (`H_max`) are **not
shipped as release assets**. Instead:

1. **Zebra source pins their hashes** (SHA-256) as reviewed constants — the
   trust root.
2. **Any synced node serves the data over P2P, generated on demand from its own
   finalized state** (each artifact is a deterministic function of the chain, so
   every honest node produces byte-identical output).
3. **Downloading nodes fetch from peers and verify against the pinned hashes.**

The `emit-snapshot` command becomes a **release-time constants-updater**: run
against a synced node, it recomputes the hashes and edits the Zebra source
constants (the last known-hash chunk hash / a new chunk hash; the max checkpoint
height; the unspent-output-set hash). It ships no asset files.

This removes ~107–240 MB of vendored assets from the repo/release and makes the
dataset grow by P2P, gated only by the small reviewed hash constants.

## 1. Components and the shared contract

Four components, which must agree on three shared artifacts:

- **A — Chunk format v2** (`zebra-chain/src/parameters/known_hashes.rs`): the
  deterministic byte layout of a chunk, now carrying per-block hash, approximate
  size hint, and the sapling/orchard tree roots at the heights in the span that
  update each tree.
- **B — Constants-updater** (`zebrad/src/commands/emit_snapshot.rs`): regenerate
  artifacts from state, hash them, edit the source constants.
- **C — P2P serve/request** (`zebra-network` + `zebrad/src/components/inbound.rs`):
  new messages to fetch chunks by index, trees by height, and the unspent-output
  set by range; content-addressed verification.
- **D — Consume** (`zebrad/src/components/ibd/`): the engine fetches + verifies
  chunks/trees/utxo-set from peers instead of reading shipped assets / folding.

### 1.1 Shared artifacts (the contract every component obeys)

1. **Chunk bytes for span `s`** (150,000 blocks): a deterministic function
   `chunk(network, s) -> Vec<u8>`. Its SHA-256 is `chunk_hashes[s]` in the spec.
   Layout (v2):
   ```
   [block hashes : n × 32]                          // existing
   [size hints   : n × 1]                           // existing (size.div_ceil(7844))
   [sapling updates: u32 count, then count × (u32 rel_height, 32B root)]   // new, sparse, ascending
   [orchard updates: u32 count, then count × (u32 rel_height, 32B root)]   // new, sparse, ascending
   ```
   The loader distinguishes v1 (`n×32` or `n×33`) from v2 by a
   `KnownHashListSpec.format` field (replaces the current fragile
   length-detection). `rel_height` is the within-span offset, so a node looking
   up the tree root for height `H` binary-searches for the largest recorded
   `rel_height ≤ (H − span_base)`.

2. **Tree-by-height payload**: the canonical serialization of a
   `sapling`/`orchard` `NoteCommitmentTree` frontier as of height `H`. Must be
   deterministic so the verifier's recomputed `.root()` matches the chunk's
   recorded root. Source: `ZebraDb::sapling_tree_by_height(&H)` /
   `orchard_tree_by_height(&H)`. Verification: deserialize → `.root()` →
   `<[u8;32]>::from(root)` == the chunk's tree root for `H`.

3. **Unspent-output-set bytes**: the sorted concatenation of 8-byte
   `OutputLocation`s from `ZebraDb::for_each_unspent_output_location_bytes` at
   `H_max`. Its SHA-256 is a new pinned constant
   (`MAINNET_UNSPENT_OUTPUTS_HASH` / `TESTNET_…`). Served ranged into ≤1 MiB
   sub-chunks (2 MiB `MAX_PROTOCOL_MESSAGE_LEN`), each verifiable by re-hashing
   the whole set after assembly (or per-range Merkle — see C).

4. **Address-balance-set bytes at `H_max`**: the sorted concatenation of
   `(transparent::Address, AddressBalanceLocation)` from
   `balance_by_transparent_addr` at `H_max` (24-byte value: balance + received +
   first-output-location). Its SHA-256 is a new pinned constant
   (`MAINNET_ADDRESS_BALANCES_HASH` / `TESTNET_…`). Served and verified like the
   unspent-output set. Loading the *final* balances sidesteps the elision
   "received-total divergence" entirely (we never recompute balances during
   sync; we load the correct final values at `H_max`).

## 1.2 The full assumeUTXO model (the consume target)

After this change, during the `0..=H_max` known-hash phase a node **validates
each block only by its pinned hash and does NOT derive state**:

- it does **not** fold note commitment trees — it **writes downloaded trees by
  height directly** into the sapling/orchard tree CFs (verified against the
  chunk's recorded root);
- it does **not** compute address balances — it loads the **final balances at
  `H_max`** directly (the per-block balance GET+merge churn, our measured
  bottleneck, is eliminated);
- it does **not** write non-survivor UTXOs — only outputs in the `H_max`
  survivor set are inserted into `utxo_by_out_loc` (elision);
- the known-hash chunks themselves are **stored in a RocksDB column family** as
  they are downloaded and verified — not read from `.bin` asset files.

At `H_max` the finalized state (trees + balances + survivor UTXOs + value pools)
is byte-identical to a normally-synced node, but it was *downloaded and written*
rather than *derived* — removing the entire state-derivation bottleneck.

## 2. P2P protocol (component C)

Anchored in the real enums/codec:

- **Internal `Request`** (`zebra-network/src/protocol/internal/request.rs:35`):
  add `KnownHashChunk(u32)`, `NoteCommitmentTree { pool: ShieldedPool, height:
  block::Height }`, `UnspentOutputs { offset: u64, len: u32 }`.
- **Internal `Response`** (`.../response.rs:20`): add `KnownHashChunk(Bytes)`,
  `NoteCommitmentTree(Bytes)`, `UnspentOutputs(Bytes)`, and a `NotAvailable`
  signal for not-yet-synced/over-limit.
- **External `Message`** (`.../external/message.rs:40`) + **codec command
  strings** (`.../external/codec.rs:154`): one 12-byte command per message,
  e.g. `getkhchunk`/`khchunk`, `getnctree`/`nctree`, `getunspent`/`unspent`.
- **Wiring** (`zebra-network/src/peer/connection.rs`): request→message and
  message→response, mirroring `BlocksByHash`/`GetData`.
- **Inbound serve** (`zebrad/src/components/inbound.rs:407`, the
  `match zn::Request` block): each new request is served by reading from
  `zebra_state` (chunk regenerated from state; tree by height; utxo-set range)
  and bounding the response under the 2 MiB frame.
- **Capability negotiation** (`PeerServices`, `peer/handshake.rs`): advertise
  support via a **reserved service bit** so the messages are only ever sent to
  Zebra peers that set it; zcashd peers (which don't) are never asked, and the
  codec already drops unknown commands. (Fallback if a spare bit is contentious:
  a user-agent substring marker.)
- **DoS bounds**: one chunk / one tree / one ≤1 MiB utxo range per request;
  frame-size check vs 2 MiB; the existing inbound rate limiter; unknown or
  above-tip heights return `NotAvailable`, never an error that drops the peer.

## 3. Consume (component D) — wiring it up everywhere

### 3.1 Known-hash chunks resident in RocksDB (not `.bin`)

A new finalized-state column family, **`known_hash_chunk`** (key: `u32` chunk
index → value: verified chunk bytes), holds the chunks. As the engine downloads
a chunk from a peer and verifies SHA-256 == the pinned `chunk_hashes[s]`, it
writes it to this CF. The loader reads hashes/sizes/tree-roots from the CF, not
from asset files. Consequences:

- The hash-source seam (`HashSource`, the generic engine's hash trait) is backed
  by the CF instead of `KnownHashList`'s file reads; the pinned `chunk_hashes`
  constants stay in `zebra-chain` as the trust root, but the *bytes* live in the
  state DB. A small read accessor (`ZebraDb::known_hash_chunk(index)`) + writer
  (`DiskWriteBatch::write_known_hash_chunk`) are added.
- Chunk residency/eviction is RocksDB's job; the windowed two-chunk RAM cache in
  `KnownHashList` is replaced by CF reads (page-cache-backed).
- Bundled/pinned-HTTPS fallback remains for cold-start before any peer serves
  them; a fetched-and-verified fallback chunk is written to the CF identically.

### 3.2 Note commitment trees written directly (no folding)

For a checkpoint block below `H_max`, the engine fetches the sapling/orchard
tree **as of that height** from a peer, verifies its `.root()` against the
chunk's recorded root for that height, and the commit **writes the downloaded
tree directly** into `sapling_note_commitment_tree` /
`orchard_note_commitment_tree` instead of folding notes. The plug-in point is the
in-memory checkpoint commit's tree update (`Chain`'s
`update_chain_tip_with_block_parallel`, the worker-thread fold this branch
identified as the bottleneck), which now takes a "tree supplied by download" path
that skips `update_trees_parallel`. The block's `hashFinalSaplingRoot` (in the
header the engine already hash-pins) is re-verified against the supplied sapling
tree before it is accepted; an unverifiable era, an absent/undeserializable tree,
or a subtree-completing height all fall back to folding (correctness), and a
contradicting Sapling root is a fatal commit error. See §8.2 item 1 for the
subtree handling.

**Tree lookahead.** Trees must be fetched *ahead of* the block download — a
deeper lookahead window for the per-height tree requests than for blocks — so
that by the time a block reaches the commit stage its tree is already downloaded
and verified, and the state's "tree supplied by download" path is taken on the
common path. If a tree has not arrived yet, the commit falls back to folding
(correct, just slower), so the lookahead is a throughput optimization, not a
correctness requirement. Only the *updating* heights need a tree (trees update
at ~7% of heights); the lookahead scheduler keys tree requests off the chunk's
sparse tree-root list (`*_root_at_or_before`), requesting the tree at each
updating height a configurable margin ahead of the commit frontier.

### 3.3 Address balances + survivor UTXOs at `H_max`

- **Address balances:** loaded once when the engine reaches `H_max` (or streamed
  in as the final artifact), verified against `…_ADDRESS_BALANCES_HASH`, and
  written directly into `balance_by_transparent_addr`. During `0..=H_max` the
  per-block balance credit/debit/merge is **skipped entirely** — this removes the
  measured "address calc + balance GET" bottleneck. Intermediate-height address
  RPC is incomplete during IBD (already accepted), and the final balances are
  exactly correct because they are the downloaded snapshot, not a derivation.
- **Survivor UTXOs:** the `H_max` unspent-output set is fetched + verified at
  startup into a memory-mapped sorted slice (`is_survivor(loc)` = binary
  search). The finalized write path inserts into `utxo_by_out_loc` **only** when
  `is_survivor(loc)` (elision). Crash-safety: because trees and balances are no
  longer derived from the UTXO set during this phase, and spend *resolution* for
  known-hash blocks does not value-check (checkpoint-verified), the
  `utxo.rs:179` fall-through hazard (`docs/design/utxo-elision.md`) is avoided
  **iff** known-hash commit truly skips transparent spend value-resolution —
  this must be verified against the commit path; otherwise fall back to the
  address-index-only Phase 1 elision there.

### 3.4 Bootstrap sequencing

Peer discovery → fetch+verify the chunk(s) covering the active window (so the
engine has hashes/sizes/tree-roots) → fetch blocks + per-height trees in
parallel → at `H_max`, load balances + finalize the survivor UTXO set. The
generic engine already sequences peer readiness and per-height fetch; chunk and
tree fetches slot onto the same weighted-fetch rails as block fetches.

## 4. Build order (dependency-first)

1. **A**: chunk format v2 + `KnownHashListSpec.format` + loader reads tree-root
   sections + unit tests (synthetic round-trip). *Foundation; everything keys
   off the chunk bytes.*
2. **B**: `emit-snapshot` regenerates the chunks (hash+size+tree-roots), the
   unspent set, and the address-balance set from the DB, computes all hashes,
   and updates the source constants (marker-anchored edits) + prints a review
   diff. Validate the regenerated v1-equivalent chunk SHAs match the existing
   pinned hashes (a correctness gate).
3. **C**: protocol messages + codec + connection wiring + inbound serve handlers
   (chunk-by-index, tree-by-height, unspent-set-range, balance-set-range) +
   capability bit. Codec round-trip + inbound-handler unit tests.
4. **D-storage**: the `known_hash_chunk` RocksDB CF + read/write accessors;
   switch the hash source from `.bin` files to the CF. Unit-testable.
5. **D-consume**: loader P2P-fetch-into-CF source; tree-load-instead-of-fold in
   the commit path; skip per-block balance derivation + load balances at
   `H_max`; survivor-only `utxo_by_out_loc` writes. Integration/sync-test-gated.

### 4.1 First end-to-end slice (smallest working vertical)

**Serve + fetch a known-hash chunk by index, content-addressed.** It exercises
A (chunk bytes), a slice of B (compute one chunk's SHA), and C (one message pair
+ inbound serve + verify), and is fully unit-testable between two in-process
peers without a live chain. Build this first, then fan out trees and the utxo
set along the same rails.

## 5. Testability

- Unit: chunk v2 round-trip; constants-updater idempotency + SHA match vs
  existing chunks; codec round-trip; inbound handler against a fake state;
  two-peer message exchange + verify.
- Integration / sync-test-gated (the user runs these): full from-scratch sync
  fetching chunks/trees/utxo-set over P2P; throughput vs the asset-shipped
  baseline; tree-load-vs-fold timing; elision UTXO-set-at-H_max parity.

## 6. Open questions (genuinely need the user)

- A spare `PeerServices` bit vs a user-agent marker for capability.
- Whether to also keep a bundled/HTTPS fallback for the chunks, or P2P-only.
- Tree-load-instead-of-fold changes the commit path's trust model (the tree is
  fetched, not derived) — acceptable for checkpoint-verified blocks below
  `H_max`, but the injection point needs review.

## 8. Implementation status, remaining work, and manual testing

### 8.1 What landed (branch `ibd-engine`, committed, build + clippy + tests green)
- **Network protocol** (`zebra-network`): `ShieldedPool`; internal
  `Request`/`Response` + external `Message`/codec for ranged known-hash chunks
  (`KnownHashChunkRange{index,offset,len}`), `NoteCommitmentTree{pool,height}`,
  and `GetSnapshot` ranges (unspent-output + address-balance sets), all under the
  2 MiB frame; `connection.rs` wiring; a `PeerServices` capability bit so only
  supporting Zebra peers are asked; size-checked responses.
- **State** (`zebra-state`): the `known_hash_chunk` rocksdb CF; chunk-v2 format
  (`ZKH2`, deterministic-from-state, sparse updating-height roots) + parser; the
  snapshot-consume write path (`SurvivorSet`, survivor-only `utxo_by_out_loc`
  elision now crash-safe, H_max bulk-load of value pools + address balances,
  direct supplied-tree write arm in the disk writer); the checkpoint
  spend-validation lookup + `PrunedChain` removed; the gated consensus/RPC
  write-thread split into a second DB.
- **IBD** (`zebrad`): the engine's `CfHashSource` (chunk fetch → verify SHA vs
  pinned hash → persist to CF), tree fetch-by-height + root verification +
  lookahead ahead of the block frontier, snapshot bootstrap; `emit-snapshot` is
  the release-time constants-updater. All snapshot-consume behavior is gated
  (default off).

### 8.2 Remaining work
1. **Supplied-tree write-through (#10) — DONE (the throughput win is active).**
   Trees are fetched, verified, buffered, threaded to the commit, and now
   **written through** instead of folded. The implementation took refined design
   (a): the in-memory checkpoint commit
   (`NonFinalizedState::commit_checkpoint_block` →
   `Chain::push_with_supplied_trees` → `update_chain_tip_with_block_parallel`)
   writes the supplied, verified sapling/orchard frontiers directly and skips the
   per-note fold (`update_trees_parallel`), gated on snapshot-consume mode and on
   the supplied trees verifying against the header (`hashFinalSaplingRoot`, the
   Sapling/Blossom era where the header pins the bare Sapling root — the same
   `service::check::supplied_trees_are_verifiable` check the finalized arm uses,
   so the two layers never disagree). Sprout is still folded (the payload carries
   only sapling/orchard; sprout has no subtree tracking and is cheap).

   **Subtrees:** the downloaded end-of-block frontier blob *cannot* reproduce a
   `2^16`-leaf subtree root that completes *mid-block* (once the frontier advances
   past the boundary, the completed subtree's internal nodes are gone — verified
   empirically against `incrementalmerkletree`'s `Frontier::root(Some(16))`).
   Subtree roots are served + RPC-checked, so they must stay byte-identical to a
   normally-synced node. The commit therefore detects a subtree completion cheaply
   (a `subtree_index` comparison via `contains_new_subtree`, no hashing) and on the
   rare height that completes one (≤ one per `2^16` notes per pool — a handful of
   heights across the whole chain) **falls back to a full fold**, the canonical
   path that produces the byte-identical subtree. This keeps the throughput win on
   the overwhelming common case while never diverging on subtree roots. A
   supplied-tree fetch that is absent, undeserializable, or unverifiable against
   the header also folds (correctness fallback), and a supplied Sapling root that
   *contradicts* the header pin is a fatal commit error (the engine refetches).
   The behaviour is observable via the `state.checkpoint.tree.{supplied,folded}`
   and `ibd.tree.{supplied,folded}` counters, and covered by H_max tree-parity,
   fold-skip, subtree-fallback, gating, and reject-on-mismatch tests in
   `zebra-state`.
2. **v2 asset re-emission.** The pinned `chunk_hashes` are currently the v1
   SHA-256 of the bundled `.bin` files; a v2 chunk includes tree roots the v1
   files lack, so real re-emission from a synced node (`emit-snapshot`) is
   required to make the content-addressing trust root v2. The
   `UNSPENT_OUTPUTS_HASH` / `ADDRESS_BALANCES_HASH` (and a value-pool-set hash)
   constants likewise do not exist until emitted. Until then snapshot-consume is
   dormant (it is gated on those hashes being `Some`).
3. Sync-test-gated items: full multi-node P2P sync, split-on RPC parity at scale,
   and the tree-load-vs-fold / elision throughput comparison.

### 8.3 How to manually test
- **Default path (unchanged):** a normal known-hash sync (`sync.known_hash_sync`)
  behaves exactly as before — the snapshot-consume features are gated off. This
  is directly testable.
- **`emit-snapshot` constants-updater:** run it against the synced testnet/mainnet
  state in `~/.cache/zebra` to recompute + edit the pinned constants (it edits
  source in place, idempotent, prints a diff). This exercises the chunk
  regeneration + the unspent/balance set hashing end to end against real state.
- **P2P serve/request + snapshot-consume:** needs (2) above (re-emit so the
  hashes are `Some`) plus at least one peer serving and a fresh-DB consumer with
  `state.snapshot_consume` configured; correctness will hold but the tree-load
  speedup waits on (1).

## 9. Local-file source (solo snapshot-consume sync, the test path)

A snapshot-consume node normally fetches the new artifacts (known-hash chunks,
note commitment trees, the unspent-output set, the address-balance set, the chain
value pools) from peers over the P2P extension above. To make the whole pipeline
testable on a **single node with no peer speaking the extension**, the consumer
can read those artifacts from **local files** instead. Blocks themselves still
come over normal P2P / known-hash; only the new artifacts come from files.

### 9.1 The single dispatch point

The fetch is factored so P2P-vs-file is one decision: `SnapshotSource`
(`zebrad/src/components/ibd/consume.rs`) has a `P2p` variant and a `LocalFiles`
variant, each exposing the same three raw-byte fetches — `chunk_range`,
`tree_bytes`, `set_range`. Everything downstream (the SHA-256 chunk check, the
tree-root-vs-chunk check, the set-SHA-256 check) is applied **identically**
regardless of source, so the local-file path and the P2P path verify against the
*same* pinned constants and are byte-for-byte equivalent. The CF-backed
`CfHashSource` holds a `SnapshotSource` for chunk fetches; the engine's
`tree_fetch_stage` holds an optional `LocalSnapshotSource` for trees (the single
tree-fetch dispatch point); `fetch_and_verify_set` takes a `SnapshotSource` for
sets. With the default config (`sync.known_hash_local_source_dir = None`) every
variant is `P2p` and behaviour is unchanged.

### 9.2 Emitting the artifact set

`emit-snapshot --emit-files --out-dir <dir>` writes the complete v2 artifact set
into `<dir>`, all bytes byte-identical to what the P2P serve path returns (the
chunk bytes come from the same `zebra_state::known_hash_chunk_bytes`; the tree
records hold the same `note_commitment_tree_bytes` serialization; the set files
hold the same sorted bytes; the value pools file holds the 40-byte
`ValueBalance::to_bytes`). Layout (also written as `MANIFEST.txt`):

```text
<dir>/
├── MANIFEST.txt                  layout + provenance (network, H_max)
├── chunks/chunk-<index>.bin      exact v2 chunk bytes per index (5-digit zero-padded)
├── sapling-trees.bin             (height u32 LE, len u32 LE, frontier-bytes)* sorted by height
├── orchard-trees.bin             same record layout for orchard
├── unspent-output-locations.bin  the sorted unspent-output-location set
├── address-balances.bin          the sorted address-balance set
└── chain-value-pools.bin         the 40-byte H_max ValueBalance
```

### 9.3 Consuming from local files

Set, in the node config:

```toml
[sync]
known_hash_sync = true
snapshot_consume_sync = true
known_hash_local_source_dir = "/path/to/<dir>"

[state]
# snapshot_consume must also be configured (survivor set, H_max, elision) —
# see the `[state] snapshot_consume` config and docs/design/utxo-elision.md.
```

With this set on a fresh DB, the engine reads each artifact from `<dir>` instead
of issuing the P2P request, verifying each against the identical pinned hashes
(chunk SHA vs `chunk_hashes[index]`; set SHA vs the pinned set hash; tree root vs
the chunk's recorded root). A wrong-hash file is rejected exactly as a corrupt
peer would be. This is the path the round-trip tests exercise
(`zebrad/src/components/ibd/consume/tests.rs` and the emit round-trip in
`zebrad/src/commands/emit_snapshot.rs`).

### 9.4 V1/V2 consistency (important)

Applying v2 constants via `emit-snapshot` makes the **bundled v1 `.bin` known-hash
assets stale**: a v2 chunk embeds the sapling/orchard tree roots a v1 chunk lacks,
so the v1 file's SHA-256 no longer equals the v2 pinned `chunk_hashes[index]`. The
local-file source (and the P2P source) supersede the bundled v1 `.bin` assets for
snapshot-consume sync — the CF-backed `CfHashSource` has **no v1 fallback** by
design (a v1/v2 disagreement at a chunk boundary would be invisible to the
synchronous lookups). So:

- The **bundled v1 `.bin` assets are not deleted** by this change (a normal,
  non-snapshot known-hash sync still uses them, gated on the v1 pinned hashes).
- For a **solo snapshot-consume test**, point `known_hash_local_source_dir` at a
  directory emitted by `emit-snapshot --emit-files` against a node synced to the
  same `H_max` the v2 constants pin. Then v2 constants + v2 files + the local
  source are mutually consistent, and the node drives the known-hash sync from the
  local v2 chunks with no peer serving the extension.
