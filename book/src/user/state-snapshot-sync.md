# Starting a pruned node from a state snapshot

A [pruned node](pruning.md) with an empty state can download a **state snapshot** from
[version 2 protocol](p2p-v2.md) peers instead of syncing the chain from genesis. The snapshot is
Zebra's finalized state at a height `H`: the UTXO set, the nullifier and anchor sets, the note
commitment trees, the value pools, the hashes and headers of the 2,304 most recent blocks, and the
body of block `H`. It ships no older block bodies, and no index the node can rebuild, such as the
address balances. It does repeat a little the node already has: the headers of its window and the
genesis block. The node first syncs and verifies the block headers from genesis, like any node.
Then it checks the snapshot against a _commitment_ and against those headers, installs it as its
state, keeps every header, and syncs only the blocks above `H`. On Mainnet that replaces about
278 GB of block history with about 2 GB. While it installs the snapshot, the node copies its synced
headers into it, so it briefly needs free disk space for a second copy of them, about 5 GB on
Mainnet, besides the snapshot.

This is not the same as the [published state archives](snapshots.md). Those are tar archives of a
complete database that you download and extract yourself. Snapshot sync happens inside Zebra, over
the peer-to-peer network, and only for pruned nodes.

Snapshot sync is **on by default** for pruned nodes, and does nothing on archive nodes. It uses the
draft version 2 protocol's `get-object` request, which can still change. This build has the
Mainnet snapshot height and block hash, but not yet its manifest hash, so it pins no Mainnet or
Testnet commitment yet, and on those networks it needs a setting that accepts a commitment of your
own (see [Where the commitment comes from](#where-the-commitment-comes-from)). A pruned node that
finds no commitment, or no version 2 peer to download from, logs why and syncs normally, so the
default never stops a node from starting.

## What is verified, and what is trusted

A snapshot is named by a commitment with three fields: the snapshot height `H`, the hash of block
`H`, and the hash of the snapshot _manifest_, which lists the hash of every piece of the snapshot.
Everything the node downloads is checked against these hashes, so a peer cannot alter a snapshot or
serve a different one. But a hash only proves that you received the snapshot the commitment names.
It does not prove that the snapshot is the real chain state. **Whoever produced the commitment is
trusted with the chain state**, in the same way the checkpoints compiled into Zebra are trusted.

Before the downloaded database is used, Zebra checks these parts against the chain:

- The hash of block `H`. It must equal Zebra's checkpoint at `H`, and be the hash at `H` on the
  header chain the node synced and verified, which reaches the next checkpoint above `H`.
- The history tree at `H`, and through it the Sapling, Orchard, and Ironwood note commitment
  trees. Block `H + 1` commits to the history tree, and the history tree commits to the tree
  roots, which Zebra recomputes from the shipped trees rather than reading cached values. Zebra
  asks its bootstrap peers for block `H + 1` until it has three structurally valid copies or has
  tried every peer, so a structurally invalid copy does not prevent it from trying the remaining
  peers. One copy that also commits to the history tree is enough. If no peer serves the block,
  the check runs when block `H + 1` is first committed.
- The window of headers `L..=H`, where `L = max(1, H − 2303)`: the headers link to each other,
  the bodies of genesis and block `H` match their transaction IDs and merkle roots, and the genesis
  block hashes to the network's genesis hash. The snapshot ships no other body.
- The headers from `H + 1` to the next checkpoint, from the synced header chain: their proof of
  work, and their difficulty and time against the window headers. Zebra commits the checked copy
  of block `H + 1` before block sync starts, so it does not download that block again.
- The headers below `L`, from the synced header chain: they must link genesis to the first
  header of the window, or the snapshot is not installed.

It also checks that the snapshot is consistent with itself: every UTXO's transaction has a
location, no two transactions share one, the UTXO total matches the transparent value pool, and
the manifest's network, genesis hash, height, and block hash match the commitment. Zebra rebuilds
the transaction hash index, and the address balances and address UTXO index, from the shipped
transaction locations and UTXO set. A snapshot that fails any check is rejected and never
installed.

These parts are **trusted on the commitment only**, and never verified later, because the node
never downloads the history below `L`:

- the UTXO set, and the transaction index rows of the transactions below `L` that created it;
- the Sprout, Sapling, Orchard, and Ironwood nullifier sets;
- the four anchor sets, including the Sprout trees they map to, and the Sprout tree;
- the value pools;
- the note commitment subtrees, and the per-height tree rows;
- the per-block value pool and size information shown by `getblock`.

A wrong or malicious commitment therefore means a wrong chain state: the node would accept spends
of outputs that do not exist, or reject outputs that do. Only use a commitment from a source you
trust with that. Sync from genesis if your security policy requires validating the chain without
trusting a snapshot producer.

### Where the commitment comes from

- A release can **pin** a commitment for a network, next to its checkpoints. This build has the
  Mainnet snapshot at checkpoint 3,522,060, but its manifest hash is still unset, so no commitment
  is pinned yet, and on Mainnet and the default Testnet snapshot sync needs the setting below.
- `[sync] state_snapshot` **configures** one. On Regtest any commitment is accepted. On Mainnet
  and the default Testnet, a configured commitment is accepted only if it equals the pinned one,
  or if `[sync] unsafe_trust_state_snapshot = true` is also set, which logs a warning at every
  start. `zebrad export-state-snapshot` prints the commitment of the snapshot it exports, in the
  form the setting takes (see [Export and serve a snapshot](#export-and-serve-a-snapshot)).

## When Zebra uses a snapshot

Zebra decides at startup, before it opens the state. Every condition must hold. The first that
fails is logged once at `info`, and the node syncs normally:

- `[sync] state_snapshot_sync = true`, the default;
- `[state] prune_blocks_older_than` is set, because only pruned nodes can start from a snapshot;
- `[state] ephemeral = false`;
- `cache_dir` holds no state database for the network, in the current format or in a previous
  format Zebra could reuse, except one that only holds the headers an earlier start synced for a
  bootstrap;
- a commitment is pinned or configured, and allowed on the network;
- `[consensus] checkpoint_sync = true`, NU5 is active at `H`, `H + 1` is at or below the last
  checkpoint Zebra knows, and the history tree at `H` is not empty;
- `[sync] state_snapshot_peers` names a peer, or `[network] v2_listen_addr` is set, so the node
  can find peers that serve snapshots among the version 2 peers it connects to;
- [zcashd-compat mode](zcashd-compat.md) is off, because it needs an archive node;
- the snapshot was not rejected by an earlier attempt on this `cache_dir`.

A node that already has a state database never bootstraps again, so a restart continues from the
installed state. Delete the state directory to start over.

## Enable snapshot sync

Add the following to `zebrad.toml`. The commitment and the peers come from whoever serves the
snapshot:

```toml
[state]
cache_dir = "/path/to/zebra-cache"
ephemeral = false
# Snapshot sync is only for pruned nodes.
prune_blocks_older_than = 10000

[sync]
# On by default.
state_snapshot_sync = true
# The commitment printed by `zebrad export-state-snapshot` on the serving node.
state_snapshot = { height = <height>, hash = "<block hash>", manifest = "<manifest hash>" }
# Version 2 peers that serve snapshots, as host:port. The port is the peer's QUIC (UDP) port.
state_snapshot_peers = ["203.0.113.10:8233"]
# What to do if the snapshot cannot be downloaded or is rejected: "auto" (the default) syncs from
# genesis instead, except that a rejected configured snapshot exits; "error" always exits;
# "genesis" always syncs from genesis. After "error" exits, an aborted download is retried on the
# next start; after a normal sync starts, the node keeps its state and does not bootstrap again.
state_snapshot_fallback = "auto"
```

Without configured snapshot peers, the node downloads from the version 2 peers it is connected to
that advertise `NODE_SYNC_ARTIFACTS` and hold the snapshot's manifest, over those connections. It
asks each such peer for the manifest's first byte as it connects, and waits up to 2 minutes for one
that has it. The peers are its `[network] initial_v2_peers`,
the peers that dialled it, and the peers it discovered. So a node with `[network] v2_listen_addr`
set needs no snapshot peers, and the usual way to name a snapshot server is
`[network] initial_v2_peers`. The snapshot peers, if set, are only used for the download, over
short-lived version 2 connections next to the node's network, and are not added to the node's
peers. Peers usually allow one connection per IP address, so a snapshot peer that the node is
also connected to may refuse the download. The node syncs the blocks above `H` from its usual
peers over either protocol. Because block `H` is recent, the node counts as near the network tip
and keeps peers that advertise `NODE_NETWORK_LIMITED` instead of `NODE_NETWORK`, such as the
pruned node that served the snapshot.

Every key is described in the [`[sync]` configuration reference](config.md#sync).

## What happens at startup

A node that bootstraps starts like any other node: it opens its state and starts its network.
Then it syncs and verifies the block headers from genesis to the checkpoint after `H`, and logs
`syncing and verifying headers from genesis before the state snapshot bootstrap`. Until it has
bootstrapped or given up, it commits only the genesis block, so it downloads no body it would not
keep. It gives up, and syncs normally from its headers, if its header chain stops growing for two
minutes, or if no connected version 2 peer advertises `NODE_SYNC_ARTIFACTS` within two minutes of
the headers reaching that checkpoint.

The bootstrap logs one line per stage at `info`, each starting with `state snapshot stage:`. In
order:

| Stage | Meaning |
| --- | --- |
| `bootstrap started` | The headers reached the checkpoint after `H`, and Zebra has peers to download from. |
| `bootstrap peers ready` | The snapshot peers are connected and advertise `NODE_SYNC_ARTIFACTS`. |
| `manifest verified` | The manifest hashes to the commitment, and matches it and the network. |
| `first piece verified` | The first piece hashed to its manifest entry. |
| `last piece verified` | Every piece hashed to its manifest entry. |
| `SST files built` | Every piece was turned into a database file. |
| `SST files ingested` | The new database holds every piece. |
| `import checks done` | The chain and self-consistency checks passed. |
| `state renamed into place` | The new database, with the synced headers, replaced the node's state. |
| `state initialized` | The state service started on it. |
| `first commit above the snapshot height` | The first block above `H` was committed. |
| `block after the snapshot height committed` | Block `H + 1` was committed, so the trees are verified. |
| `last checkpoint reached` | The node is past its checkpoints and verifies blocks fully. |
| `sync target reached` | The node reached a configured sync target. |

While it runs, each attempt uses a fresh, uniquely named workspace under
`<cache_dir>/state-snapshot-import/`. After success, an abort, or a rejection, Zebra deletes only
that attempt's workspace. It never deletes the shared import root or a sibling workspace, which
another bootstrap could still be using. If cleanup fails, Zebra logs a warning and leaves that
attempt's workspace in place; later attempts use fresh sibling directories. The manifest must
arrive within a minute of the first connection, and the download is abandoned if no piece completes
for five minutes or every peer has been excluded. A peer that serves bytes with the wrong hash is
disconnected and excluded for the rest of the bootstrap; a peer that does not have an object is
retried later.

If the snapshot's contents fail a check, the snapshot is **rejected**: Zebra records the manifest
hash under `<cache_dir>/state-snapshots/<network>/rejected/` and does not try that snapshot again
in this `cache_dir`. A download that cannot complete is **aborted** and records nothing. In both
cases `[sync] state_snapshot_fallback` decides what happens next. With the default `"auto"`, Zebra
continues with a normal sync from the headers it synced, unless the snapshot was configured in
`[sync] state_snapshot` and rejected, which exits like `"error"`. With `"error"`, Zebra exits with
an actionable message. An aborted download leaves a state with only the synced headers, which the
next start bootstraps over; a rejected snapshot still exits on later starts until you configure a
different trusted commitment, or investigate the failure and remove its rejection marker. With
`"genesis"`, Zebra continues with a normal sync and does not retry a rejected snapshot. A normal
sync commits blocks to the state, so after `"auto"` or `"genesis"` falls back, later starts
continue that sync and never bootstrap: delete the state directory to try the snapshot again.

## After the bootstrap

The node's blocks start at the snapshot height `H`, but it keeps every header from genesis:

- `getblockchaininfo` reports `pruned: true` and `pruneheight` equal to `H`, even before the
  node's own pruning deletes anything. Genesis is available separately below it.
- The blocks from `H` to the tip are complete. The blocks from `L` to `H − 1` have hashes and
  headers, so `getblockheader` answers for them, but no bodies. Blocks below `L` only have their
  headers, which `getblockheader` returns with `verbose = false`; the node has no other data for
  them. The
  node never downloads the missing bodies: there is no backfill. Requests for them fail, as they
  do on a [pruned node](pruning.md#rpc-and-peer-behaviour), and `gettxout` returns `null` for
  `version` when the creating transaction is below `H`.
- Outputs created below `H` are spendable, because the UTXO set is complete at `H`.
- `getaddressbalance` omits `received`, because the node does not know what an address received
  and spent below `H`. `getaddresstxids` returns the transactions from `H + 1` on.
- It advertises no service until it holds the recent blocks it would promise. With
  `prune_blocks_older_than` of at least 2,304, it advertises `NODE_NETWORK_LIMITED` to the version
  2 peers it connects to once its tip is 2,304 blocks above `H`, plus the non-finalized depth
  (`MAX_BLOCK_REORG_HEIGHT`), about three days after `H` on Mainnet. Connections made
  earlier keep the services they started with. Until then, peers that require a service cannot
  download blocks from it, including the blocks it mines. It never advertises `NODE_NETWORK`.
- The database records where its history starts, so a restart continues from the installed state
  without downloading anything again. Its format version is 33.0.0 in `state/v33`: never open it
  with an older Zebra build, and never move it to `v32`, `v31`, `v30`, `v29`, or any older
  directory. Those builds cannot decode its records, and the oldest cannot tell that the history
  below `L` is missing.

Pruning, `copy-state`, zcashd-compat mode, and the `indexer` feature treat a snapshot-born
database like a pruned one: see [pruned mining nodes](pruning.md).

## Export and serve a snapshot

Any node that holds the complete finalized state at `H`, pruned or not, can export a snapshot. It
needs about the snapshot's size in free disk space under `cache_dir`.

1. Choose `H`. On Mainnet and Testnet it should be a checkpoint height, and `H + 1` must be at or
   below the last checkpoint of the Zebra release the downloading nodes run, or they refuse the
   snapshot. NU5 must be active at `H`. On Regtest, any height above the first 2,304 blocks works
   if the downloading node's checkpoints include `H` and a height above it.
2. Stop the node with its finalized tip exactly at `H`. Set `[state] debug_stop_at_height` to `H`
   and start the node: it exits when block `H` is finalized, which is about 1,000 blocks after the
   chain reaches `H` (the non-finalized depth, `MAX_BLOCK_REORG_HEIGHT`). Pruning never deletes
   the finalized tip's body, so any pruned node works, as long as it has the block hashes from
   `L` (a node that bootstrapped from an older snapshot has them from its own window start).
3. Run the export with the node stopped, using the same Zebra version as the node, on a state that
   was never built with the `indexer` feature:

   ```sh
   zebrad --config /path/to/zebrad.toml export-state-snapshot \
     --height <height> --commitment-out /path/to/commitment.toml
   ```

   The command opens the state read-only. It refuses a state whose tip is not `H`, one without
   the block hashes from `L`, one built with the `indexer` feature, and one that needs a database
   migration. On
   success it writes the manifest and pieces to `<cache_dir>/state-snapshots/<network>/objects/`,
   each under its SHA-256 hash. `--commitment-out` writes only this complete TOML document, so a
   script can parse it without filtering export logs:

   ```toml
   # Add this table to zebrad.toml, after the keys of its [sync] section:
   [sync.state_snapshot]
   height = <height>
   hash = "<block hash>"
   manifest = "<manifest hash>"
   ```

   Without `--commitment-out`, the same table is printed to stdout, which can also contain export
   logs. Failures are printed to the terminal's standard error stream and return a nonzero exit
   status.

4. Serve the store. Remove `debug_stop_at_height`, open a version 2 endpoint, and turn serving on:

   ```toml
   [network]
   v2_listen_addr = "[::]:8233"

   [sync]
   serve_state_snapshots = true
   ```

   At startup the node re-hashes every object in the store before offering it. Only regular files
   of at most 16 MiB are considered; rejected manifests and pieces of manifests it cannot verify
   are left out. The scan runs in the background: the node starts without waiting for it, and
   serves the store's objects once it finishes. Like every node with `v2_listen_addr` set, the
   node advertises `NODE_SYNC_ARTIFACTS` to version 2 peers whether or not its store holds a
   snapshot, and answers requests for objects it does not hold as not found.

   The node serves at most two concurrent transfers per peer and eight in total; peers asked to
   wait retry with a backoff. A bootstrapping node connects with a short-lived connection that
   advertises no services, in addition to its normal connections. Zebra limits the connection
   attempts it accepts from one IP address within about two minutes, so a serving node behind the
   same IP address as its clients needs `[network] max_connections_per_ip` of at least 4: the
   bootstrap, version 2, and legacy connections, with room for one restart.

5. Publish the commitment through a channel your users trust, because the commitment is what they
   trust (see [What is verified, and what is trusted](#what-is-verified-and-what-is-trusted)).

A node that bootstrapped from a snapshot keeps the downloaded pieces only when
`serve_state_snapshots` is on, and then serves them too. Otherwise it deletes them after the
import.

## Limitations

- The `get-object` request is a draft, and the snapshot format can change between releases: a
  snapshot exported by one Zebra version may be refused by another. This version reads and writes
  snapshot format 2.
- Mainnet and the default Testnet need a pinned commitment, or `unsafe_trust_state_snapshot`.
  Custom testnets cannot use it, because the version 2 protocol has
  [no ALPN identifier for them](p2p-v2.md#known-limitations).
- The bodies below `H` are never downloaded. A node that needs them must sync from genesis or
  restore an [archive](snapshots.md) instead.
- A snapshot-born node keeps the headers below `L`, but does not serve them to peers yet.
- Without configured snapshot peers, the node needs `[network] v2_listen_addr` to find
  `NODE_SYNC_ARTIFACTS` peers, and it advertises the services of a snapshot node from its start,
  even if it then gives up and syncs normally, until it restarts.
- While it syncs headers for a bootstrap, the node also connects to peers that don't serve old
  blocks, since it only needs their headers. If it then gives up and syncs from genesis, those
  connections can slow the sync until they are replaced by peers that serve old blocks.
- A node that bootstrapped from a snapshot does not serve
  [known hashes](p2p-v2.md#serving-known-hashes): it answers every `get-hashes` request with no
  entries, because its per-block counts would need the history below `L`.
- The export runs on a stopped node whose finalized tip is exactly `H`. A running node cannot
  export, and a node past `H` cannot go back to it.
