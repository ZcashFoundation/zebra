# Starting a pruned node from a state snapshot (experimental)

A [pruned node](pruning.md) with an empty state can download a **state snapshot** from
[version 2 protocol](p2p-v2.md) peers instead of syncing the chain from genesis. The snapshot is
Zebra's finalized state at a height `H`: the UTXO set, the nullifier and anchor sets, the note
commitment trees, the value pools, the address indexes, and the 2,304 most recent blocks. The node
checks the snapshot against a _commitment_, installs it as its state, and then syncs only the
blocks above `H`. On Mainnet that replaces about 278 GB of block history with a few GB.

This is not the same as the [published state archives](snapshots.md). Those are tar archives of a
complete database that you download and extract yourself. Snapshot sync happens inside Zebra, over
the peer-to-peer network, and only for pruned nodes.

Snapshot sync is **experimental** and **off by default**. It uses the draft version 2 protocol's
`get-object` request, which can still change, and no release pins a Mainnet or Testnet commitment
yet, so on those networks it needs a setting that accepts a commitment of your own (see
[Where the commitment comes from](#where-the-commitment-comes-from)).

## What is verified, and what is trusted

A snapshot is named by a commitment with three fields: the snapshot height `H`, the hash of block
`H`, and the hash of the snapshot _manifest_, which lists the hash of every piece of the snapshot.
Everything the node downloads is checked against these hashes, so a peer cannot alter a snapshot or
serve a different one. But a hash only proves that you received the snapshot the commitment names.
It does not prove that the snapshot is the real chain state. **Whoever produced the commitment is
trusted with the chain state**, in the same way the checkpoints compiled into Zebra are trusted.

Before the downloaded database is used, Zebra checks these parts against the chain:

- The hash of block `H`. It must equal Zebra's checkpoint at `H`, or chain through headers to the
  next checkpoint above `H`. If no peer serves those headers, the checkpoint verifier checks it
  when it reaches that checkpoint, and stalls with a specific error if it does not match.
- The history tree at `H`, and through it the Sapling, Orchard, and Ironwood note commitment
  trees. Block `H + 1` commits to the history tree, and the history tree commits to the tree
  roots, which Zebra recomputes from the shipped trees rather than reading cached values. Zebra
  asks its bootstrap peers for block `H + 1` until it has three structurally valid copies or has
  tried every peer, so a structurally invalid copy does not prevent it from trying the remaining
  peers. One copy that also commits to the history tree is enough. If no peer serves the block,
  the check runs when block `H + 1` is first committed.
- The window of blocks `L..=H`, where `L = max(1, H − 2303)`: the headers link to each other, each
  body's transaction IDs and merkle root match, and the genesis block hashes to the network's
  genesis hash.

It also checks that the snapshot is consistent with itself: every UTXO has its transaction index
rows and address rows, every address balance is the sum of that address's UTXOs, the UTXO total
matches the transparent value pool, and the manifest's network, genesis hash, height, and block
hash match the commitment. A snapshot that fails any check is rejected and never installed.

These parts are **trusted on the commitment only**, and never verified later, because the node
never downloads the history below `L`:

- the UTXO set, and the transaction index rows of the transactions below `L` that created it;
- the Sprout, Sapling, Orchard, and Ironwood nullifier sets;
- the four anchor sets, including the Sprout trees they map to, and the Sprout tree;
- the value pools;
- the address balances and address indexes;
- the note commitment subtrees, and the per-height tree rows;
- the per-block value pool and size information shown by `getblock`;
- the authorizing data (signatures and proofs) of the window blocks, which the node serves to
  peers on the strength of the commitment.

A wrong or malicious commitment therefore means a wrong chain state: the node would accept spends
of outputs that do not exist, or reject outputs that do. Only use a commitment from a source you
trust with that. Sync from genesis if your security policy requires validating the chain without
trusting a snapshot producer.

### Where the commitment comes from

- A release can **pin** a commitment for a network, next to its checkpoints. No release pins one
  yet, so on Mainnet and the default Testnet snapshot sync needs the setting below.
- `[sync] state_snapshot` **configures** one. On Regtest any commitment is accepted. On Mainnet
  and the default Testnet, a configured commitment is accepted only if it equals the pinned one,
  or if `[sync] unsafe_trust_state_snapshot = true` is also set, which logs a warning at every
  start. `zebrad export-state-snapshot` prints the commitment of the snapshot it exports, in the
  form the setting takes (see [Export and serve a snapshot](#export-and-serve-a-snapshot)).

## When Zebra uses a snapshot

Zebra decides at startup, before it opens the state. Every condition must hold. The first that
fails is logged once at `info`, and the node syncs normally:

- `[sync] state_snapshot_sync = true`;
- `[state] prune_blocks_older_than` is set, because only pruned nodes can start from a snapshot;
- `[state] ephemeral = false`;
- `cache_dir` holds no state database for the network, in the current format or in a previous
  format Zebra could reuse;
- a commitment is pinned or configured, and allowed on the network;
- `[consensus] checkpoint_sync = true`, NU5 is active at `H`, `H + 1` is at or below the last
  checkpoint Zebra knows, and the history tree at `H` is not empty;
- `[sync] state_snapshot_peers`, or when it is empty `[network] initial_v2_peers`, names a peer;
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
state_snapshot_sync = true
# The commitment printed by `zebrad export-state-snapshot` on the serving node.
state_snapshot = { height = <height>, hash = "<block hash>", manifest = "<manifest hash>" }
# Version 2 peers that serve snapshots, as host:port. The port is the peer's QUIC (UDP) port.
state_snapshot_peers = ["203.0.113.10:8233"]
# What to do if the snapshot cannot be downloaded or is rejected: "error" (the default) exits;
# "genesis" syncs from genesis instead. An aborted download is retried on the next start.
state_snapshot_fallback = "error"
```

The snapshot peers are only used for the download, over a short-lived version 2 connection that
Zebra opens before its network starts. They are not added to the node's peers. To keep syncing
blocks from them over the version 2 protocol afterwards, also set `[network] v2_listen_addr` and
list them in `[network] initial_v2_peers`. The node syncs the blocks above `H` from its usual
peers over either protocol. Because block `H` is recent, the node counts as near the network tip
and keeps peers that advertise `NODE_NETWORK_LIMITED` instead of `NODE_NETWORK`, such as the
pruned node that served the snapshot.

Every key is described in the [`[sync]` configuration reference](config.md#sync).

## What happens at startup

The bootstrap logs one line per stage at `info`, each starting with `state snapshot stage:`. In
order:

| Stage | Meaning |
| --- | --- |
| `bootstrap started` | The conditions above hold, so Zebra bootstraps from the commitment. |
| `bootstrap peers ready` | The snapshot peers are connected and advertise `NODE_SYNC_ARTIFACTS`. |
| `manifest verified` | The manifest hashes to the commitment, and matches it and the network. |
| `first piece verified` | The first piece hashed to its manifest entry. |
| `last piece verified` | Every piece hashed to its manifest entry. |
| `SST files built` | Every piece was turned into a database file. |
| `SST files ingested` | The new database holds every piece. |
| `import checks done` | The chain and self-consistency checks passed. |
| `state renamed into place` | The new database is now the node's state. |
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
cases `[sync] state_snapshot_fallback` decides what happens next. With `"error"`, Zebra exits with
an actionable message. An aborted download leaves no state,
so the next start retries it; a rejected snapshot still exits on later starts until you configure a
different trusted commitment, or investigate the failure and remove its rejection marker. With
`"genesis"`, Zebra continues with a normal sync from genesis and does not retry a rejected
snapshot.

## After the bootstrap

The node's history starts at the window start `L`:

- `getblockchaininfo` reports `pruned: true` and `pruneheight` equal to `L`, even before the
  node's own pruning deletes anything. Genesis is available separately below it.
- The blocks from `L` to the tip are complete. Blocks below `L` have no bodies, and the node
  never downloads them: there is no backfill. Requests for them fail, as they do on a
  [pruned node](pruning.md#rpc-and-peer-behaviour), and `gettxout` returns `null` for `version`
  when the creating transaction is below `L`.
- Outputs created below `L` are spendable, because the UTXO set is complete at `H`.
- With `prune_blocks_older_than` of at least 2,304 the node advertises `NODE_NETWORK_LIMITED` to
  version 2 peers from its first start, because the snapshot ships the most recent 2,304 blocks
  and pruning never retains fewer. It never advertises `NODE_NETWORK`.
- The database records where its history starts, so a restart continues from the installed state
  without downloading anything again. Its format version is 33.0.0 in `state/v33`: never open it
  with an older Zebra build, and never move it to `v32`, `v30`, `v29`, or any older directory.
  Those builds cannot decode its records, and the oldest cannot tell that the history below `L` is
  missing.

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
   chain reaches `H` (the non-finalized depth, `MAX_BLOCK_REORG_HEIGHT`). The node must not have
   pruned below `L` when it stops. Pruning keeps every body from `finalized tip − retention + 1`
   up, so a node that stops with its finalized tip at `H` keeps the whole window with any
   retention of at least 2,304. Every retention the
   [pruning page](pruning.md#enable-pruning-and-choose-retention) allows on Mainnet and Testnet
   (at least 10,000) satisfies that; Regtest allows retentions as small as 1,001, so use at least
   2,304 there.
3. Run the export with the node stopped, using the same Zebra version as the node, on a state that
   was never built with the `indexer` feature:

   ```sh
   zebrad --config /path/to/zebrad.toml export-state-snapshot \
     --height <height> --commitment-out /path/to/commitment.toml
   ```

   The command opens the state read-only. It refuses a state whose tip is not `H`, one pruned
   below `L`, one built with the `indexer` feature, and one that needs a database migration. On
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

- Snapshot sync is experimental. The `get-object` request is a draft, and the snapshot format can
  change between releases: a snapshot exported by one Zebra version may be refused by another.
- Mainnet and the default Testnet need a pinned commitment, or `unsafe_trust_state_snapshot`.
  Custom testnets cannot use it, because the version 2 protocol has
  [no ALPN identifier for them](p2p-v2.md#known-limitations).
- The history below `L` is never downloaded. A node that needs it must sync from genesis or
  restore an [archive](snapshots.md) instead.
- Snapshot peers are not discovered: `NODE_SYNC_ARTIFACTS` peers must be configured.
- A node that bootstrapped from a snapshot does not serve
  [known hashes](p2p-v2.md#serving-known-hashes): it answers every `get-hashes` request with no
  entries, because its per-block counts would need the history below `L`.
- The export runs on a stopped node whose finalized tip is exactly `H`. A running node cannot
  export, and a node past `H` cannot go back to it.
