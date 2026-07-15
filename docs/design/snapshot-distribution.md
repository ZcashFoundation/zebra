# Distribution of Known-Hash Chunks, Note Commitment Trees, and the Unspent-Output Set

Status: **IMPLEMENTED (installer-based distribution; snapshot-consume path
dormant pending v2 asset re-emission).** Base: branch `ibd-engine`. See §8 for
what landed, the remaining work, and how to manually test.

## 0. The architecture decision

The known-hash chunks, the selected note commitment tree frontiers, and the set
of unspent transparent output locations at the max checkpoint height (`H_max`)
are **shipped as release artifacts** (on zfnd.org or the GitHub releases) and
**downloaded by the installer** into a local artifact directory:

1. **Zebra source pins their hashes** (SHA-256) as reviewed constants — the
   trust root.
2. **`emit-snapshot --emit-files`, run against a synced node at release time,
   emits the artifact set** (each artifact is a deterministic function of the
   chain, so every release manager produces byte-identical output) and, as the
   constants-updater, recomputes the hashes and edits the Zebra source
   constants.
3. **The installer downloads the artifacts from the release assets; the
   consuming node reads them from the local directory and verifies every
   artifact against the pinned hashes** before trusting it, so a corrupt or
   tampered download is rejected exactly as a corrupt peer would have been.

An earlier revision of this design distributed the artifacts over a P2P
network-protocol extension (new `getkhchunk`/`getnctree`/`getsnapshot`
messages, serve-from-state inbound handlers, and a capability bit). That
extension is **deferred to a future PR** — this PR ships no network protocol
changes; the artifact downloads are the installer's job, and the node consumes
local files only. The verification model is unchanged: the pinned constants
remain the trust root regardless of how the bytes arrive.

## 1. Components and the shared contract

Four components, which must agree on three shared artifacts:

- **A — Chunk format v2** (`zebra-chain/src/parameters/known_hashes.rs`): the
  deterministic byte layout of a chunk, now carrying per-block hash, approximate
  size hint, and the sapling/orchard tree roots at the heights in the span that
  update each tree.
- **B — Emitter / constants-updater** (`zebrad/src/commands/emit_snapshot.rs`):
  regenerate artifacts from state, hash them, edit the source constants, and
  write the release artifact set (`--emit-files`).
- **C — Distribution**: the artifact set is published as release assets
  (zfnd.org / GitHub releases) and downloaded by the **installer** into the
  local artifact directory. (An earlier revision served the artifacts over a
  P2P protocol extension; that is deferred to a future PR — see §0.)
- **D — Consume** (`zebrad/src/components/ibd/`): the engine reads + verifies
  chunks/trees from the artifact directory instead of folding; the state loads
  the sets at open.

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
   (`MAINNET_UNSPENT_OUTPUTS_HASH` / `TESTNET_…`), verifiable by re-hashing the
   whole artifact file.

4. **Address-balance-set bytes at `H_max`**: the sorted concatenation of
   `(transparent::Address, AddressBalanceLocation)` from
   `balance_by_transparent_addr` at `H_max` (24-byte value: balance + received +
   first-output-location). Its SHA-256 is a new pinned constant
   (`MAINNET_ADDRESS_BALANCES_HASH` / `TESTNET_…`), verified like the
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
  they are read and verified — not read from `.bin` asset files on every run.

At `H_max` the finalized state (trees + balances + survivor UTXOs + value pools)
is byte-identical to a normally-synced node, but it was *downloaded and written*
rather than *derived* — removing the entire state-derivation bottleneck.

## 2. Distribution (component C)

The emitted artifact set (§9.2) is published with each release — on zfnd.org or
as GitHub release assets — and the **installer** downloads it into the local
artifact directory (`sync.known_hash_local_source_dir`) before the node's first
start. The node never fetches snapshot artifacts over the network itself; every
artifact is verified against the pinned constants as it is read, so the
installer's transport (HTTPS) does not extend the trust model.

A P2P network-protocol extension for fetching the same artifacts from peers
(new request/response messages, serve-from-state inbound handlers, a capability
bit) was implemented in an earlier revision of this branch and is **deferred to
a future PR**: it is a strict superset of the installer flow (the same emitted
bytes, the same verification), so it can be layered back on without changing
the artifact contract.

## 3. Consume (component D) — wiring it up everywhere

### 3.1 Known-hash chunks resident in RocksDB (not `.bin`)

A new finalized-state column family, **`known_hash_chunk`** (key: `u32` chunk
index → value: verified chunk bytes), holds the chunks. As the engine reads a
chunk from the artifact directory and verifies SHA-256 == the pinned
`chunk_hashes[s]`, it writes it to this CF. The loader reads
hashes/sizes/tree-roots from the CF, not from asset files. Consequences:

- The hash-source seam (`HashSource`, the generic engine's hash trait) is backed
  by the CF instead of `KnownHashList`'s file reads; the pinned `chunk_hashes`
  constants stay in `zebra-chain` as the trust root, but the *bytes* live in the
  state DB. A small read accessor (`ZebraDb::known_hash_chunk(index)`) + writer
  (`DiskWriteBatch::write_known_hash_chunk`) are added.
- Chunk residency/eviction is RocksDB's job; the windowed two-chunk RAM cache in
  `KnownHashList` is replaced by CF reads (page-cache-backed).
- Cold-start reads come from the installer-downloaded artifact directory; a
  read-and-verified chunk is written to the CF so later runs never re-read the
  directory.

### 3.2 Note commitment trees written directly (no folding)

For a checkpoint block below `H_max`, the engine reads the sapling/orchard
tree **as of that height** from the artifact directory, verifies its `.root()`
against the chunk's recorded root for that height, and the commit **writes the
supplied tree directly** into `sapling_note_commitment_tree` /
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

**Tree lookahead.** Trees must be read *ahead of* the block download — a
deeper lookahead window for the per-height tree reads than for blocks — so
that by the time a block reaches the commit stage its tree is already read
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
- **Survivor UTXOs:** the `H_max` unspent-output set is loaded + verified at
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

Installer download → read+verify the chunk(s) covering the active window (so
the engine has hashes/sizes/tree-roots) → fetch blocks over P2P + read
per-height trees in parallel → at `H_max`, load balances + finalize the
survivor UTXO set. The generic engine already sequences peer readiness and
per-height fetch; chunk and tree reads slot in ahead of the commit frontier.

## 4. Build order (dependency-first)

1. **A**: chunk format v2 + `KnownHashListSpec.format` + loader reads tree-root
   sections + unit tests (synthetic round-trip). *Foundation; everything keys
   off the chunk bytes.*
2. **B**: `emit-snapshot` regenerates the chunks (hash+size+tree-roots), the
   unspent set, and the address-balance set from the DB, computes all hashes,
   and updates the source constants (marker-anchored edits) + prints a review
   diff. Validate the regenerated v1-equivalent chunk SHAs match the existing
   pinned hashes (a correctness gate).
3. **C**: publish the emitted artifact set with the release; the installer
   downloads it into the artifact directory. (The P2P serve/fetch protocol is
   deferred to a future PR.)
4. **D-storage**: the `known_hash_chunk` RocksDB CF + read/write accessors;
   switch the hash source from `.bin` files to the CF. Unit-testable.
5. **D-consume**: loader read-into-CF source over the artifact directory;
   tree-load-instead-of-fold in the commit path; skip per-block balance
   derivation + load balances at `H_max`; survivor-only `utxo_by_out_loc`
   writes. Integration/sync-test-gated.

### 4.1 First end-to-end slice (smallest working vertical)

**Emit + read back a known-hash chunk by index, content-addressed.** It
exercises A (chunk bytes), a slice of B (compute one chunk's SHA + emit the
file), and D (read the file, verify, persist to the CF), and is fully
unit-testable against a temp directory without a live chain. Build this first,
then fan out trees and the sets along the same rails.

## 5. Testability

- Unit: chunk v2 round-trip; constants-updater idempotency + SHA match vs
  existing chunks; emit → local-source read-back byte parity; wrong-hash /
  wrong-root artifact rejection.
- Integration / sync-test-gated (the user runs these): full from-scratch sync
  consuming a downloaded artifact set; throughput vs the asset-shipped
  baseline; tree-load-vs-fold timing; elision UTXO-set-at-H_max parity.

## 6. Open questions (genuinely need the user)

- Where the release artifacts live (zfnd.org vs GitHub release assets) and
  how the installer names/versions them.
- Tree-load-instead-of-fold changes the commit path's trust model (the tree is
  supplied, not derived) — acceptable for checkpoint-verified blocks below
  `H_max`, but the injection point needs review.

## 8. Implementation status, remaining work, and manual testing

### 8.1 What landed (branch `ibd-engine`, committed, build + clippy + tests green)
- **Network protocol**: none — the P2P snapshot-distribution extension that an
  earlier revision of this branch implemented (ranged chunk / tree / set
  request-response messages, inbound serve handlers, a capability bit) was
  removed in favour of installer-downloaded release artifacts, and is deferred
  to a future PR.
- **State** (`zebra-state`): the `known_hash_chunk` rocksdb CF; chunk-v2 format
  (`ZKH2`, deterministic-from-state, sparse updating-height roots) + parser; the
  snapshot-consume write path (`SurvivorSet`, survivor-only `utxo_by_out_loc`
  elision now crash-safe, H_max bulk-load of value pools + address balances,
  direct supplied-tree write arm in the disk writer); the checkpoint
  spend-validation lookup + `PrunedChain` removed; the gated consensus/RPC
  write-thread split into a second DB.
- **IBD** (`zebrad`): the engine's `CfHashSource` (chunk read from the
  artifact directory → verify SHA vs pinned hash → persist to CF), tree
  read-by-height + root verification + lookahead ahead of the block frontier,
  snapshot bootstrap; `emit-snapshot` is the release-time constants-updater and
  artifact emitter. All snapshot-consume behavior is gated (default off) and
  requires `sync.known_hash_local_source_dir` (the installer's download
  directory).

### 8.2 Remaining work
1. **Supplied-tree write-through (#10) — DONE (the throughput win is active).**
   Trees are read, verified, buffered, threaded to the commit, and now
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
   supplied-tree read that is absent, undeserializable, or unverifiable against
   the header also folds (correctness fallback), and a supplied Sapling root that
   *contradicts* the header pin is a fatal commit error (the engine re-reads).
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
3. Sync-test-gated items: a full sync consuming a downloaded artifact set,
   split-on RPC parity at scale, and the tree-load-vs-fold / elision throughput
   comparison.

### 8.3 How to manually test
- **Default path (unchanged):** a normal known-hash sync (`sync.known_hash_sync`)
  behaves exactly as before — the snapshot-consume features are gated off. This
  is directly testable.
- **`emit-snapshot` constants-updater:** run it against the synced testnet/mainnet
  state in `~/.cache/zebra` to recompute + edit the pinned constants (it edits
  source in place, idempotent, prints a diff). This exercises the chunk
  regeneration + the unspent/balance set hashing end to end against real state.
- **Snapshot-consume sync:** needs (2) above (re-emit so the hashes are
  `Some`) plus an artifact directory (from the installer, or emitted locally
  with `--emit-files`) and a fresh-DB consumer with
  `sync.known_hash_local_source_dir` + `state.snapshot_consume` configured.

## 9. The local-file source (the artifact directory)

A snapshot-consume node reads the artifacts (known-hash chunks, note commitment
trees, the unspent-output set, the address-balance set, the chain value pools)
from **local files**: the directory the installer downloaded, or one emitted
locally by `emit-snapshot --emit-files` (which also makes the whole pipeline
testable on a single node). Blocks themselves still come over normal P2P /
known-hash; only the snapshot artifacts come from files.

### 9.1 The single dispatch point

The reads are factored through single dispatch points so the verification is
one code path: `LocalSnapshotSource` (`zebrad/src/components/ibd/consume/local.rs`)
wraps the artifact directory; the CF-backed `CfHashSource` reads whole chunk
files through it (bounded by the maximum valid chunk size) and SHA-256-checks
them against the pinned hashes; the engine's `tree_fetch_stage` reads trees
through it (the records file is parsed once and cached; each tree is
root-checked against the chunk); the state loads and applies the sets at open
(`[state] snapshot_consume`). Everything downstream is applied **identically**
no matter where the directory came from, so a tampered download is rejected by
the same pinned constants. A deterministic artifact failure (a missing,
corrupt, or hash-mismatched file) is a fatal diagnostic the operator must act
on — the directory is read-only, so restarting can never cure it. With the
default config (`sync.known_hash_local_source_dir = None`) snapshot-consume
sync is unavailable and normal sync is unchanged.

### 9.2 Emitting the artifact set

`emit-snapshot --emit-files --out-dir <dir>` writes the complete v2 artifact
set into `<dir>`, deterministically from the finalized state (the chunk bytes
come from `zebra_state::known_hash_chunk_bytes`; the tree records hold the
`note_commitment_tree_bytes` serialization; the set files hold the sorted set
bytes; the value pools file holds the 40-byte `ValueBalance::to_bytes`), so
every release manager emits byte-identical assets. Layout (also written as
`MANIFEST.txt`):

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

With this set on a fresh DB, the engine reads each artifact from `<dir>`,
verifying each against the pinned hashes (chunk SHA vs `chunk_hashes[index]`;
set SHA vs the pinned set hash; tree root vs the chunk's recorded root). A
wrong-hash file is rejected regardless of how it got into the directory. This is
the path the round-trip tests exercise
(`zebrad/src/components/ibd/consume/tests.rs` and the emit round-trip in
`zebrad/src/commands/emit_snapshot.rs`).

### 9.4 V1/V2 consistency (important)

Applying v2 constants via `emit-snapshot` makes the **bundled v1 `.bin` known-hash
assets stale**: a v2 chunk embeds the sapling/orchard tree roots a v1 chunk lacks,
so the v1 file's SHA-256 no longer equals the v2 pinned `chunk_hashes[index]`. The
artifact-directory source supersedes the bundled v1 `.bin` assets for
snapshot-consume sync — the CF-backed `CfHashSource` has **no v1 fallback** by
design (a v1/v2 disagreement at a chunk boundary would be invisible to the
synchronous lookups). So:

- The **bundled v1 `.bin` assets are not deleted** by this change (a normal,
  non-snapshot known-hash sync still uses them, gated on the v1 pinned hashes).
- For a **solo snapshot-consume test**, point `known_hash_local_source_dir` at a
  directory emitted by `emit-snapshot --emit-files` against a node synced to the
  same `H_max` the v2 constants pin. Then v2 constants + v2 files + the local
  source are mutually consistent, and the node drives the known-hash sync from
  the local v2 chunks.
