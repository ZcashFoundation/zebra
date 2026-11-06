# Spentness hints (experimental)

Spentness hints make the initial sync of a new node write less. Below the final
checkpoint, Zebra normally writes every transparent output to its UTXO set, and
deletes it again when a later block spends it. Most outputs below the final
checkpoint are spent before it. A spentness-hint artifact marks, for every
transparent output up to the final checkpoint, whether it is still unspent
there. With it, Zebra writes only the outputs that survive, and keeps the
others in memory, or in a temporary journal, until their spends arrive.

Everything else in the state stays exact: blocks, transactions, address
balances and indexes, note commitment trees, and value pools. When the sync
reaches the final checkpoint, the state is the same as a state synced without
hints.

Hints are off by default. They are experimental, and they need the
[version 2 peer-to-peer protocol](p2p-v2.md) and its checkpoint sync engine.

## How the artifact is trusted

Each Zebra release that supports hints pins one artifact per network: its
terminal checkpoint height, the block hash at that height, and the SHA-256 of
the whole artifact. Zebra downloads the artifact from version 2 peers that
serve it, and uses it only if:

- its SHA-256 equals the pinned hash;
- its header names the pinned height and hash, and the network's genesis block;
- its bit count equals the number of transparent outputs up to the pinned
  height, as counted by the known-hash lists that Zebra already verifies.

All of these checks happen before the first block above genesis is committed.
A peer that sends a complete artifact with the wrong hash is penalized. A
verified artifact carries exactly the trust of the checkpoint list that Zebra
already uses: a release that pins a false artifact could equally pin a false
checkpoint.

Until a release pins an artifact for a network, Zebra syncs that network
without hints. Regtest and custom testnets never have hints.

## Enabling hints

```toml
[network]
# Version 2 peers serve the artifact. See the version 2 protocol page.
v2_listen_addr = "[::]:8233"

[sync]
# Download checkpointed blocks from version 2 peers.
v2_checkpoint_sync = true

# "off" (the default), "auto", or "require".
v2_spentness_hints = "auto"
```

Hints apply only to a state that starts empty. Zebra never starts using hints
part way through an existing state: with any mode, an existing state that was
synced without hints, or that finished its hinted sync, syncs without them.

With `"auto"`, Zebra waits up to five minutes after startup for the known-hash
lists to count the outputs up to the final checkpoint, then downloads the
artifact, and syncs without hints if any of this fails. With `"require"`, Zebra
waits until it has a verified artifact, and stops with an error if the release
pins none for the network or acquisition fails.

Without `v2_checkpoint_sync`, `"auto"` syncs without hints, and `"require"`
stops Zebra at startup.

After acquisition, Zebra keeps the verified artifact in the `spentness-hints`
directory of the state's `cache_dir`. If Zebra restarts before the hinted sync
reaches the final checkpoint, it loads the artifact from that directory, or
downloads it again if the directory is gone, and resumes the hinted sync, with
`"auto"` or `"require"`. Do not set `v2_spentness_hints = "off"` or
`v2_checkpoint_sync = false` until the sync passes the final checkpoint: Zebra
cannot continue a hinted sync without the artifact, and stops with an error.

## Serving the artifact

A node with `v2_listen_addr` set advertises the `NODE_SYNC_ARTIFACTS` service
to its version 2 peers, and answers their `get-object` requests, with any
mode. If the `spentness-hints` directory holds the artifact for the current
release's pin, Zebra checks it at startup and serves it; otherwise it answers
that it does not have the artifact. See
[Serving objects](p2p-v2.md#serving-objects) for the limits.

Zebra does not build the artifact from its own state. So the only nodes that
serve it are the nodes that downloaded it for their own hinted sync, and the
nodes that release maintainers seeded with it (see below). Until enough of
those are reachable, `"auto"` syncs without hints, and `"require"` keeps
waiting.

If construction finds that the artifact does not match the chain, Zebra stops
with an error. That can only happen with a defective release. Delete the state,
and sync again with `v2_spentness_hints = "off"`.

## Generating an artifact (release maintainers)

The `spentness-hints` tool in `zebra-utils` generates and audits artifacts. It
reads the state of a node that fully validated the chain, and never changes it.

```sh
cargo install --locked --features spentness-hints --path zebra-utils --bin spentness-hints
```

1. Stop a fully validated archive node, and copy its state into a separate
   state that ends exactly at the chosen final checkpoint `H`:

   ```sh
   spentness-hints --network mainnet replay \
       --source /path/to/validated/cache_dir \
       --destination /path/to/frozen/cache_dir \
       --height H --block-hash <hash at H>
   ```

   A destination that already holds an earlier part of the chain resumes from
   its tip.

2. Generate the artifact:

   ```sh
   spentness-hints --network mainnet generate \
       --state /path/to/frozen/cache_dir \
       --height H --block-hash <hash at H> \
       --output mainnet-spentness-hints.bin
   ```

   The tool prints the output and survivor counts, the artifact's SHA-256, and
   the `MaxCheckpoint` constant to pin in
   `zebra-chain/src/parameters/spentness_hints.rs`.

3. Repeat steps 1 and 2 on a second, independently validated archive. The two
   artifacts must be byte-identical.

4. Verify the artifact against an independent transparent replay of the chain:

   ```sh
   spentness-hints --network mainnet verify \
       --state /path/to/frozen/cache_dir \
       --artifact mainnet-spentness-hints.bin \
       --sha256 <sha256> --height H --block-hash <hash at H>
   ```

   `verify` rebuilds the UTXO set from the blocks in a scratch database under
   `$TMPDIR`, and checks every bit against it and against the archive's own
   UTXO set. Without `--sha256`, it uses the pin built into the binary.

5. Pin the artifact: add the `MaxCheckpoint` constant that `generate`
   printed to `MaxCheckpoint::for_network` in
   `zebra-chain/src/parameters/spentness_hints.rs`, and make sure `H` is in
   the network's checkpoint list. Until a release does this, hints do
   nothing on that network: every node syncs without them, whatever its
   mode.

6. Seed a few version 2 nodes with the artifact, so that new nodes can
   download it. On each node, which must run the release that pins it and set
   `v2_listen_addr`, install the file as
   `<cache_dir>/spentness-hints/<sha256>`, with the state's `cache_dir` and
   the artifact's SHA-256 in lowercase hex, then restart the node:

   ```sh
   mkdir -p /path/to/cache_dir/spentness-hints
   cp mainnet-spentness-hints.bin /path/to/cache_dir/spentness-hints/<sha256>
   ```

   These nodes advertise `NODE_SYNC_ARTIFACTS` and serve the artifact from
   then on. Nodes that download it serve it too.

   Zebra never creates the first copy itself, so this step is required: a
   release that pins an artifact nobody seeded makes every `"auto"` node
   sync without hints, and every `"require"` node wait.

### Before the first release that pins an artifact

No release pins an artifact yet, so these limits have only been tested, not
tuned on a real network. Review them, with the artifact's real size, before
the first pinning release:

- an artifact download drops a peer that averages less than 256 KiB/s
  (`MIN_RATE` in `zebrad/src/components/checkpoint_sync/hints.rs`);
- with `"auto"`, Zebra gives up on hints if the known-hash lists have not
  counted the outputs within five minutes (`KNOWN_HASH_SUM_TIMEOUT`), or if
  the artifact download takes more than five minutes (`AUTO_TIMEOUT`); the
  legacy syncer waits five minutes more than both before it syncs without
  hints (`LEGACY_SYNC_HINTS_TIMEOUT`);
- after a restart that interrupted a hinted sync, the state's writer waits
  up to ten minutes for the artifact before it stops Zebra
  (`RESUME_ARTIFACT_TIMEOUT` in
  `zebra-state/src/service/finalized_state/zebra_db/spentness.rs`).
