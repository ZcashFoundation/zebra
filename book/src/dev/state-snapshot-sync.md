# State-snapshot sync

A pruned Zebra node with an empty state downloads a verified state snapshot at a height `H` over
the version 2 peer-to-peer protocol, installs it, and syncs only the blocks above `H`. This page is
the design, trimmed to what a contributor needs. The operator view is in
[Starting a pruned node from a state snapshot](../user/state-snapshot-sync.md).

The drafts it implements are the sync draft (zips#1346, "Snapshot Synchronization") and the
transport draft (zips#1344, `get-object`, stream type `0x09`, service flag `NODE_SYNC_ARTIFACTS`).
Where this design and a draft disagree, the design wins; see
[Differences from the drafts](#differences-from-the-drafts).

## Scope

In scope: the `NODE_SYNC_ARTIFACTS` service flag and `get-object`; a deterministic snapshot
format, an offline exporter, and an offline verified importer; a bootstrap phase in `zebrad start`
before `zebra_state::init`; the state changes for a database whose history starts at a window
below `H`; config, docs, unit tests, a Regtest end-to-end test, and a Mainnet benchmark.

Out of scope, for follow-ups: live export at pinned heights; backfill below `H`; the drafts'
`get-block-range`, `get-hashes`, `get-tree-roots`, and hints; pinned Mainnet and Testnet
commitments; chain-verifying the authorizing data of the window bodies; dynamic service flags.

## Trust model and verification

The **commitment** is `StateSnapshotCommitment { height: H, block_hash: hash(H), manifest }`
(`zebra-chain/src/state_snapshot.rs`). It is pinned per network through
`Network::state_snapshot_commitment()`, which is `None` until a release pins one, or configured in
`[sync] state_snapshot`. A configured commitment is accepted on Regtest and custom testnets; on
Mainnet and the default Testnet only if it equals the pinned one or
`[sync] unsafe_trust_state_snapshot` is set, which logs a warning at every start. A pinned
commitment wins otherwise.

**Preconditions**, checked by the decision rule and again by the importer:
`consensus.checkpoint_sync` is on; NU5 is active at `H`; `H + 1 ≤ max_checkpoint_height` (the
router's effective value); the history tree at `H` is non-empty. A release pins `H` at a
checkpoint height at or below the last
checkpoint minus one, cut together with the checkpoint list.

**Verified against the chain**, all before the database is renamed into place:

1. `hash(H)`: when `H` is a checkpoint height it must equal the binary's checkpoint; otherwise the
   bootstrap fetches headers `H+1..=C` (`C` the first checkpoint above `H`) from the bootstrap peers
   and hash-chains them from `hash(H)` to `C`. If no peer serves them, the check is deferred to the
   checkpoint verifier, which anchors `C` back to `hash(H)` and stalls on a mismatch with a specific
   error log. The deferral is recorded in the marker's `deferred_checks`.
2. The history tree, and through it the frontiers. The importer decodes the shipped history tree and
   requires `current_height == H`, a rightmost peak with `end_height == H`
   (`NonEmptyHistoryTree::last_peak`), and `network_upgrade == current(H)`. It decodes the Sapling,
   Orchard, Ironwood, and Sprout tip trees, recomputes their roots ignoring cached roots, and
   requires `end_sapling_root` and `end_orchard_root` to match, `end_ironwood_root` to match for V3
   nodes from NU6.3 (ZIP 258), an empty Ironwood state below NU6.3, and every tip root to be in its
   anchor set.
3. Block `H+1`, requested from eligible bootstrap peers until three structurally valid bodies have
   been retained or every peer has been tried: the header chains to `hash(H)`, the merkle root
   matches, and `hashBlockCommitments(H+1) = BLAKE2b("ZcashBlockCommit", history_root(H) ‖
   authDataRoot(H+1) ‖ 0^32)`. Structurally invalid bodies do not count toward the three-copy
   limit, so they cannot hide a valid body from a later peer. One copy that also matches the
   commitment is enough; mismatching copies blame only their peers. If no peer serves `H+1`, the
   check is deferred to the first commit above `H`, which runs it, and a failure there resets and
   retries like normal sync, never rejecting automatically.
4. The window: `L == max(1, H − 2303)`; headers `L..=H` link by `previous_block_hash`; each window
   body's recomputed txids match its `hash_by_tx_loc` rows and its merkle root; genesis rows hash to
   `network.genesis_hash()`.

**Trusted on the manifest only**, never discharged (there is no backfill): the UTXO set and its
transaction-index rows below `L`; the four nullifier sets; the four anchor sets including the
Sprout root → tree map; the Sprout tree; the value pools; the address indexes including `received`
totals; the subtrees and the per-height tree rows in and below the window; `block_info`; the window
bodies' authorizing data. A node serves the window blocks on the strength of the commitment.

**Self-consistency checks**, each refusing the import: every UTXO's transaction location has a
`hash_by_tx_loc` ↔ `tx_loc_by_hash` pair; every UTXO with an address has a balance row and an
address-UTXO row; each balance equals the sum of its address's UTXOs and every address-UTXO row
points at an existing UTXO; Σ UTXO values equals the transparent pool;
`tip_chain_value_pool == block_info[H].value_pools`; the manifest's network, genesis, `H`, and
`hash(H)` match the commitment; piece sizes and entry counts match the parsed records.

## Snapshot contents

The window `L..=H` is archive-complete, so a snapshot-born node serves 2,304 recent blocks and can
advertise `NODE_NETWORK_LIMITED`. Genesis is shipped too. Blocks below `L` have no bodies and no
hash rows.

Components are column-family rows in the current database format's encoding, in this fixed order
(`zebra_state::state_snapshot::format::ComponentId`, 28 ids): `hash_by_height`, `height_by_hash`,
`block_header_by_height`, `tx_by_loc`, `hash_by_tx_loc`, `tx_loc_by_hash`, `block_info`,
`utxo_by_out_loc`, `balance_by_transparent_addr`, `utxo_loc_by_transparent_addr_loc`,
`tx_loc_by_transparent_addr_loc`, the four `*_nullifiers`, the four `*_anchors`, the three
`*_note_commitment_tree`, the three `*_note_commitment_subtree`, `sprout_note_commitment_tree`,
`history_tree`, `tip_chain_value_pool`. The indexer's `tx_loc_by_spent_out_loc` is never shipped.

**Canonical values.** A component whose value has more than one on-disk encoding is decoded with
`FromDisk` and re-encoded with the current `IntoDisk`, with cached roots always recomputed:
`block_info`, `tip_chain_value_pool`, `history_tree`, `balance_by_transparent_addr`, and every tree
value (the Sapling, Orchard, and Ironwood trees, the Sprout tip tree, and the Sprout anchor trees).
Every other component is copied raw. The importer re-encodes the tree values the same way and never
trusts a cached root. Determinism therefore holds across databases with different upgrade
histories, and a test checks it.

## Formats

All integers are little-endian; CompactSize is per ZIP 204, canonical only; hashes are SHA-256.
`zebra_state::state_snapshot::format` implements both encodings and their strict decoders.

A **piece** is a sequence of records `CompactSize(key_len) ‖ key ‖ CompactSize(value_len) ‖ value`,
strictly increasing by key, all from one component, split at record boundaries into pieces of at
most `MAX_PIECE_BYTES = 16 MiB`. A single record over the limit is an export error. An empty
component has no pieces. `PieceWriter` splits on export; `PieceReader` checks canonical lengths,
bounds before allocation, the running total, and key order on import.

The **manifest** (at most 1 MiB) is:

```text
magic "ZEBRA-STATE-SNAPSHOT" ‖ snapshot_format u16 (1)
genesis_hash 32 ‖ height u32 ‖ block_hash 32 ‖ window_start u32 ‖ db_format_major u16 ‖ index_mode u8 (0)
[Regtest and custom testnets only: parameters_hash 32, the consensus parameters without checkpoints]
component_count u16 (28), then per component in ComponentId order:
  component_id u16 ‖ entry_count u64 ‖ piece_count u32 ‖ pieces[ size u32 ‖ sha256 32 ]
```

Decoding is strict: bounds before allocation, no trailing bytes, no unknown ids, every component
exactly once in order, `entry_count == 0 ⇔ piece_count == 0`. The decoder takes the `Network`,
because the parameters hash is present only on Regtest and custom testnets. `snapshot_format`
changes only when shipped encodings change; the importer maps it onto the running database format.

The manifest and the optional version 2 handshake parameters record use the same hash. It binds
every consensus parameter, including activation heights, funding streams, lockbox disbursements,
and coinbase-spending rules. Checkpoint lists are deliberately excluded because they are Zebra's
local verification anchors, not consensus rules, and newer releases routinely extend them. Binding
the lists would reject an otherwise compatible snapshot whenever the exporter and importer had
different checkpoint list lengths. The importer's own checkpoints still authenticate the snapshot
block hash and its header chain as described above.

The **artifact store** (`zebra_state::state_snapshot::store::ArtifactStore`) keeps objects at
`<state cache_dir>/state-snapshots/<network.directory_name()>/objects/<sha256-hex>`, written as a
temporary file, fsynced, and renamed, and rejection marks at `.../rejected/<manifest-hex>`.
`contains_verified` re-hashes a file before trusting it.

## Protocol

`get-object` and its index come from the checkpointed initial block download stack, which also
serves the [spentness-hint artifact](../user/spentness-hints.md) through them; the user-facing
limits are in [Serving objects](../user/p2p-v2.md#serving-objects). In `zebra-network`:

- `PeerServices::NODE_SYNC_ARTIFACTS = 1 << 4` is version 2 only: `Config::advertised_services`
  strips it (with `NODE_NETWORK_LIMITED`) from legacy `version` messages, and
  `Config::v2_advertised_services` keeps it for `init` records, and adds it for a node with an
  object source.
- `Request::Object { hash, offset, length }`, `Response::Object { size, data }`, and
  `Response::ObjectNotFound` are the internal request and responses. `ObjectHash` lives in
  `zebra-chain` next to the commitment and is re-exported. `LoadTrackedClient::supports` routes
  `Object` only to version 2 peers that advertise bit 4; the legacy connection refuses it with
  `PeerError::V2OnlyRequest`.
- Wire: the request is `hash 32 ‖ offset u64 ‖ length u32` (44 bytes), with `offset ≤ 2^48 − 1`
  and `length ≤ 32 MiB`, otherwise `PROTOCOL_ERROR`; the requester checks its own requests too.
  The response is `result` (`0x00` found, `0x02` not found) ‖ CompactSize `size` ‖ data until FIN.

**Requester** (`peer/v2/connection/outbound.rs`): streams the data and never reserves from `size`;
treats more than `length` bytes, or data past `size`, as `FLOOD`; keeps a bounded (1,024-entry)
per-connection map of the `size` seen per hash and closes with `PROTOCOL_ERROR` when it changes;
once `result` and `size` have arrived, a deadline, reset, or `CANCELLED` returns the prefix read so
far as a short answer, not a failure.

**Server** (`peer/v2/connection/inbound/object.rs`, outside the shared `Inbound` buffer): reads an
`ObjectSource`, a newtype around a `watch::Receiver<Arc<ObjectIndex>>` of `ObjectHash → (path,
size)` (a newtype rather than an alias, so `zebra_network::Config` can stay `Eq`), held in a
runtime-only `zebra_network::Config` field and passed down to `ConnectionParams`. `serve_object`
answers `REFUSED` unless this node advertises bit 4; answers probes (`length == 0` or
`offset ≥ size`) and not-found from the index without touching disk; reads data with positional
reads in `spawn_blocking`, in 1 MiB chunks; takes permits with `try_acquire` under
`MAX_OBJECT_STREAMS_PER_PEER = 2` and `MAX_OBJECT_STREAMS_GLOBAL = 8`, answering excess requests
with `REFUSED` counted as busy; bounds the bytes by the request's `length`; writes FIN early, at
its request deadline minus 4 s, and never resets after `result` and `size` are written. The serving
node's QUIC `send_window` is 8 MiB.

**Bootstrap dialer** `zebra_network::v2_bootstrap_clients(config, peers, user_agent)`
(`peer_set/initialize/v2.rs`): requires `NODE_SYNC_ARTIFACTS` of every peer, through a new
handshake parameter, where normal dialers keep requiring `NODE_NETWORK` while syncing; uses a
client-only endpoint per address family with QUIC windows of 8 MiB per stream and 16 MiB per
connection; presents `LocalIdentity { services: empty, relay: false, start_height: 0 }`, an
inbound service that refuses everything, a dummy inventory collector, and `MisbehaviorScores` with
a dropped receiver; deduplicates peers by IP, at most 8. `BootstrapPeers::close` drops the clients,
closes the endpoint with `NO_ERROR`, and waits 2 s for `wait_idle` before the network starts. The
serving node sees a zero-service inbound peer.

## Bootstrap in zebrad

`zebrad/src/components/state_snapshot/` holds `decision.rs` (the rule), `bootstrap.rs` (the flow),
`scheduler.rs` (the piece downloader), `progress.rs` (the `Stage` log lines), and `serving.rs` (the
object index). The flow runs in `StartCmd` before `zebra_state::init`.

**Decision rule** (`should_bootstrap`), every condition logged once at `info` when it fails:
`[sync] state_snapshot_sync`; pruning configured; the state not ephemeral; no current-major database
directory and no restorable previous-major one (`restorable_db_versions` + on-disk format probes); a
commitment available and allowed; the preconditions above; `[sync] state_snapshot_peers` or, when
empty, `network.initial_v2_peers` not empty; zcashd-compat off; the manifest not marked rejected.

**Flow**:

1. Create a fresh, uniquely named workspace for this attempt under
   `<cache_dir>/state-snapshot-import/`. Never delete the shared import root or a sibling workspace,
   because another bootstrap could still be using it.
2. Dial the bootstrap peers.
3. Fetch the manifest with `get-object(manifest, 0, 1 MiB)`; abort if `size` > 1 MiB; a hash
   mismatch blames the peer and tries the next; check the manifest against the commitment and the
   network.
4. Run the `hash(H)` check and fetch block `H+1`, with `get-headers` and `get-blocks` on the
   bootstrap clients.
5. Fetch the pieces with the scheduler, while a bounded blocking pool
   (`available_parallelism() / 2`) turns each verified piece into one SST built with
   `DiskDb::options()`.
6. Import: ingest, write the singletons, the marker, and the version file; run the
   self-consistency, chain, and format validity checks synchronously, mapping any failure to a
   refusal; fsync; clear the stale non-finalized backup; rename into place.
7. Delete the pieces unless `serve_state_snapshots` is set, and delete this attempt's workspace.
   Close the bootstrap endpoint, then continue the normal start. Cleanup failures are logged without
   removing the shared import root or any sibling workspace.

**Failure handling**: a peer-attributable failure blames the peer and excludes it for the
bootstrap (a single peer's hash mismatch closes its connection with `MISBEHAVIOR`; `FLOOD` and
`PROTOCOL_ERROR` are closed by the connection layer; not-found means a 30 s per-(peer, hash)
backoff). The manifest must arrive within 60 s of the first handshake; the piece phase is aborted
if no piece verifies for 5 min or every peer is excluded. On an abort, or on a rejected snapshot (a
verified manifest whose contents fail the checks), the rejection is recorded when it is the
snapshot's fault, only this attempt's workspace is deleted, and `[sync] state_snapshot_fallback`
applies.
`"error"` exits with an actionable message: an aborted download leaves no state directory and is
retried on the next start, while a rejected manifest remains fatal on later starts until its marker
is removed after investigation or a different commitment is configured. `"genesis"` continues with
a normal sync from genesis without retrying a rejected snapshot. `BootstrapError::Rejected` marks
the manifest; `BootstrapError::Aborted` marks nothing.

**Scheduler** (`Scheduler<S>`, generic over
`S: Service<zn::Request, Response = zn::Response, Error = BoxError>`, peers keyed by
`PeerSocketAddr`, tested with mock services): one owning task with a `FuturesUnordered` and no
mutexes; each piece goes to one peer; sub-ranges of `clamp(ewma_rate × 8 s, 256 KiB, 4 MiB)`,
starting at 1 MiB, never past the manifest's size, at most 2 in flight per peer; a sub-range that
returns less than `min(64 KiB, requested)` before the end of the object is a short answer, and 3 in
a row means stalled; a peer whose EWMA stays below 64 KiB/s for 30 s is stalled; in the end-game a
peer below ¼ of the median loses its piece to an independent whole-piece duplicate; a stalled
peer's piece is reassigned whole, its partial data discarded, the peer gets a 60 s cooldown and is
excluded after 3 stalls; `REFUSED` backs off exponentially from 1 s to 30 s with no penalty; the
global budget is 256 MiB of in-flight plus unverified bytes and at most 8 peers; each piece is
hashed incrementally; a `size` that differs from the manifest cancels that peer's request with no
penalty. `SchedulerConfig` carries these values.

## State changes

- A new column family `snapshot_base` holds `()` → `SnapshotBase { window_start, height,
  block_hash, manifest, deferred_checks }` (71 bytes on disk). The intermediate 30.1.0 upgrade adds
  the column family without rewriting records because `create_missing_column_families` already
  creates it. The current 32.0.0 major format puts snapshot-born databases in `state/v32`, enforcing
  the compatibility barrier instead of relying on operators not to open them with older 30.x builds.
  Existing v30 and older registered sources move to v32 and run every intervening upgrade. Format
  v31 remains reserved for #12182's variable-height record migration. Snapshot-born databases must
  not be renamed to v30, v31, v29, or any older directory: those readers cannot tell that the
  history below `L` is missing.
- `HistoryCapabilities` gains `lowest_known_height` and `snapshot_base: Option<SnapshotBase>`. With
  a marker, the retained floor is `max(L, first body)` and the database counts as pruned.
- Fixes: `legacy_chain` and `PruningWitness::update` treat reaching the snapshot base as the
  witness; the `block_info` validity check starts at `max(tip − 1000, L)`; `prune_batch` gets a
  no-progress guard; `ZebraDb::transaction` maps a location below `L` to `BlockBodyUnavailable`;
  `spent_outputs_for_block` maps an unknown parent with a marker to `BlockBodyUnavailable`; a
  snapshot database requires pruning. `zebra-consensus` starts its state checkpoint validation at
  the first checkpoint at or above `L`. `zebra-rpc` `getblock` reads the parent `BlockInfo` by
  height when the lookup by hash misses, because `block_info[L − 1]` is shipped but has no hash row.
  `zebrad` skips `request_genesis` when the state has a tip.
- **Importer** (`zebra_state::state_snapshot::import`): temp root
  `<cache_dir>/state-snapshot-import/<nonce>/` holding `state/v32/<net>`; a raw RocksDB handle with
  all column families and `DiskDb::options()`; one SST ingest per column family with `move_files`,
  into empty column families at the bottom level, auto-compactions off while ingesting; singletons
  written by batch; the marker, then the version file; close, then reopen as `ZebraDb` with a temp
  `Config` (`cache_dir` the temp root, pruning set, `delete_old_database = false`) so it opens as
  `CheckOpenCurrent`; run the checks; close, fsync, and rename the leaf directory to
  `config.db_path(..)`.
- **Exporter** (`zebra_state::state_snapshot::export`, run by `zebrad export-state-snapshot`):
  opens the state read-only as a secondary instance and refuses unless `can_read_without_migration`
  holds; refuses unless the tip is `H`, the database is not pruned below `L`, and there is no
  indexer; exports each column family on its own blocking thread with raw iterators and readahead,
  canonicalizing where the contents section says; uses a merge join for the transaction-index
  subset below `L` and targeted gets for the tree rows. The command prints the complete
  `[sync.state_snapshot]` table, or writes it without interleaved logs to `--commitment-out PATH`;
  export failures go to standard error.

## Serving

`[sync] serve_state_snapshots = true` adds the artifact store's objects to the node's
`ObjectIndex` (`components/state_snapshot/serving.rs`), which also holds the spentness-hint
artifact when the node serves one. Each regular file of at most `MAX_OBJECT_BYTES` (16 MiB)
is re-hashed against its file name in the background before it is published; rejected manifests and
the pieces of unverified manifests are excluded. Startup does not wait for the scan, and the index
gains the store's objects when it finishes. The service bit does not depend on the scan:
`Config::v2_advertised_services` adds `NODE_SYNC_ARTIFACTS` for every node with an object source,
which every version 2 node has, and objects outside the index are answered as not found. Reporting a `Stage` also increments
`sync.state_snapshot.stages.total` with the stage text as its bounded `stage` label.

## Configuration

The `[sync]` keys `state_snapshot_sync`, `state_snapshot`, `unsafe_trust_state_snapshot`,
`state_snapshot_peers`, `state_snapshot_fallback`, and `serve_state_snapshots` are documented in
the [configuration reference](../user/config.md#sync). The five keys with non-optional defaults are
stored in `zebrad/tests/common/configs/v7.1.0.toml`. The optional `state_snapshot` commitment is
absent there because its default is `None`, which TOML has no value for. The export command tests
cover its `[sync.state_snapshot]` table; the configuration reference shows the inline-table form.

## Tests and benchmark

Unit tests cover: manifest and piece strictness and bounds; export determinism across database
lineages; an import round trip on a populated Regtest-style database, with each tampered input (the
history tree, a frontier, a cached root, the UTXO set, an address balance, a piece byte, a window
body) refused at import; the subtree checks on a sparse exported database; `get-object` wire
vectors; routing; `serve_object` limits, early FIN, and `REFUSED` without bit 4; the bootstrap
dialer accepting a pruned server with bit 4 while the normal dialer rejects it; the scheduler
against mock peers that are slow, lying, not-found, stalled, trickling one byte, finishing early,
or refusing.

The Regtest end-to-end test is `zebrad/tests/integration/regtest/state_snapshot.rs`, ignored by
default. Node A is pruned with a retention of at least 2,304, stops at a finalized tip of `H`,
exports, and restarts serving with `max_connections_per_ip = 4` and a version 2 listener, then
mines past `H + 1000`. Node B starts empty and pruned with the exported commitment and Regtest
checkpoints generated from A's chain (genesis, `H`, and heights from `H + 2`), bootstraps over
loopback, and reaches A's tip. It checks the pruned history, spends a UTXO created below `L`,
restarts without bootstrapping again, compares the bytes it kept against A's state size, and
matches the stage lines in order. A third node without checkpoints is refused by the decision rule
and syncs from genesis through full verification.

The completed Mainnet benchmark served a snapshot exported from a hard-linked copy of a Mainnet
state advanced to a checkpoint height with `debug_stop_at_height`, then synced to a fixed target
cross-checked against an archive. Its review evidence records bytes, CPU, peak RSS, write bytes,
QUIC statistics, blocks per second, and restarts. The `zebra.net.out.objects.bytes` comparisons are
exact object response payload measurements: they exclude request bytes, response framing, QUIC
packets, and UDP/IP overhead, and are not total-wire-byte or wire-overhead measurements.

## Differences from the drafts

The implementation differs from the public drafts in these contributor-relevant ways:

- The sync draft's minimum contents cannot validate blocks above `H`. Zebra also ships the four
  anchor sets, the transaction index of every UTXO-creating transaction, the block at `H` and
  genesis, recent headers, and the subtrees, and trusts them on the manifest.
- Verifying the history tree at `H` needs block `H+1`'s `hashAuthDataRoot`, not only its header
  (ZIP 244), so Zebra fetches the whole block.
- The manifest format is unspecified in the draft; Zebra's binary manifest is implementation-local,
  like the checkpoint commitment scheme.
- The draft's piece size (16–64 MiB) exceeds a `get-object` `length` (32 MiB); Zebra uses pieces of
  at most 16 MiB and fetches sub-ranges.
- The draft has no per-request deadline for `get-object`; Zebra's version 2 connection uses a 20 s
  whole-request deadline, so the requester treats a late deadline as a short answer and the
  scheduler sizes sub-ranges to finish inside it.
- The `get-object` size-consistency rule's scope is unspecified; Zebra tracks it per connection,
  bounded to 1,024 hashes, and closes with `PROTOCOL_ERROR` on a change and `FLOOD` on data past
  `size`.
- A snapshot-born node advertises `NODE_NETWORK_LIMITED` on the strength of the commitment, which
  the draft's "verified authorizing data" rule for serving blocks may or may not allow.
