# Version 2 peer-to-peer protocol (beta)

Zebra includes an opt-in, beta implementation of the draft version 2 Zcash
peer-to-peer protocol, which runs over QUIC instead of TCP. The draft is
[`draft-arya-jvff-p2p-quic-transport`][draft] in the ZIPs repository. It is
still a draft: its wire format can change, and Zebra's implementation covers
only the parts needed to relay blocks, transactions and addresses.

[draft]: https://github.com/zcash/zips/blob/main/zips/draft-arya-jvff-p2p-quic-transport.md

The version 2 protocol is **off by default**. A node that does not enable it
behaves exactly as before, and a node that enables it keeps speaking the legacy
TCP protocol on `network.listen_addr`, so it stays compatible with every other
Zcash node.

## Enabling it

Set a UDP listen address in the `[network]` section of `zebrad.toml`, and,
optionally, list peers to connect to over the version 2 protocol:

```toml
[network]
# The legacy TCP protocol keeps running here.
listen_addr = "[::]:8233"

# Opens a QUIC (UDP) endpoint for the version 2 protocol. The draft expects
# the network's default port, 8233 on Mainnet and 18233 on Testnet.
v2_listen_addr = "[::]:8233"

# Peers to connect to over the version 2 protocol at startup, as host:port.
# The port is the peer's QUIC (UDP) port.
initial_v2_peers = ["203.0.113.10:8233", "v2-peer.example.com:8233"]

# Discover more version 2 peers (the default).
v2_peer_discovery = true
```

`v2_listen_addr` can use the same port number as `listen_addr`, because one is
TCP and the other is UDP. If the node is behind a firewall or NAT, forward UDP
on that port as well as TCP.

An IPv6 address such as `[::]:8233` reaches both IPv6 and IPv4 peers on Linux
and macOS. An IPv4 address such as `0.0.0.0:8233` reaches only IPv4 peers, so
Zebra skips IPv6 addresses in `initial_v2_peers` with a warning. On Windows,
`[::]` reaches only IPv6 peers.

Version 2 connections join the same peer set as legacy connections, so the
syncer, the mempool, and the inbound service use them like any other peer. The
connection metrics count them too, with their own handshake stages, so a failed
QUIC handshake is counted separately from a failed TCP one. They are not listed
by the `getpeerinfo` RPC yet, because that lists the legacy address book, and
version 2 peers are recorded in a separate version 2 address book.

## What works

- Accepting version 2 connections, and connecting to the configured
  `initial_v2_peers`. Configured peers are reconnected after a disconnection,
  after the reconnection delay plus a random extra of up to a quarter of it. A
  configured peer that already has a version 2 connection to this node is not
  dialled. When the configured hosts resolve to more addresses than the
  outbound version 2 limit, each host gets a connection before any host gets a
  second one, and a failed or closed connection's slot goes to the next host
  or address in turn. Zebra keeps a slot free for each configured host that
  has not resolved yet, so a host that resolves late still gets one, but a host
  that never resolves leaves its slot unused. Connections that stay up keep
  their slots, so if there are more configured hosts than slots, the hosts that
  resolve first get them, and the others wait for a connection to fail or end.
- Serving and requesting headers, blocks, and transactions. Zebra answers
  block and transaction requests in request order, up to about 1 MB per
  response.
- Serving `get-block-range` requests, which a syncing peer uses to download
  up to 64 MiB of consecutive blocks in one stream. See
  [Serving block ranges](#serving-block-ranges).
- Serving `get-hashes` requests, which a peer uses to fetch the hash, size,
  and transaction, transparent output, and note counts of up to 50,000
  consecutive blocks. See [Serving known hashes](#serving-known-hashes).
- Serving `get-object` requests, which a peer uses to download a
  synchronization artifact, such as the spentness-hint artifact, by its
  SHA-256 hash. See [Serving objects](#serving-objects).
- Downloading the checkpointed part of the chain with `get-block-range`, when
  `sync.v2_checkpoint_sync` is set. See
  [Checkpoint sync over version 2](#checkpoint-sync-over-version-2).
- Exchanging peer addresses as the draft recommends: Zebra asks for addresses
  once, on connections it made, and answers once, on connections it accepted.
- Discovering version 2 peers, unless `v2_peer_discovery` is `false`. Version 2
  addresses are QUIC endpoints, so Zebra keeps them in a version 2 address
  book, apart from the legacy one: the configured peers, the addresses version
  2 peers send in answers and announcements, and the version 2 peers Zebra
  connected to. Relayed addresses are bucketed by their address group and the
  group of the peer that sent them, so one network range cannot fill the book,
  and they never displace the configured, cached, or seeded addresses, or the
  peers Zebra connected to. Zebra answers version 2 address requests from this
  book only, with addresses it connected to or recently heard of over version
  2, and its own version 2 address when it has a specific one
  (`v2_listen_addr`, or the IP address of `external_addr` with the
  `v2_listen_addr` port). It keeps the version 2 peers it connected to most
  recently in `<cache_dir>/network/<network>.v2-peers`, next to the legacy peer
  cache, and tries them at startup. It never caches an address it only heard
  of. Zebra dials the peers it connected to before first, then picks at random
  among the others, giving seeded and cached addresses even odds against
  relayed ones, whatever time the relaying peer claimed. When it knows too few, it looks up the DNS seeders in
  `initial_mainnet_peers` or `initial_testnet_peers` and dials the results on
  the network's default port, UDP 8233 on Mainnet and 18233 on Testnet, at most
  every 30 minutes. Configured peers keep the first claim on the outbound
  version 2 slots; discovered peers get up to half of the initial target size,
  at most one per address group. Few nodes accept QUIC yet, so most DNS seed
  results fail to connect, which Zebra logs at the `debug` level.
- Relaying version 2 addresses. Zebra relays each fresh address a version 2
  peer announces to two of its other version 2 peers, and limits each peer to
  about one announced address every 10 seconds, dropping the rest. It also
  advertises its own version 2 address to each version 2 peer soon after
  connecting and then about once a day: the IP address of `external_addr` with
  the port of `v2_listen_addr` if `external_addr` is set, otherwise
  `v2_listen_addr` if it is a specific address. An unspecified listener, like
  `[::]:8233`, is not advertised.
- Transaction announcements in both directions. Block and address
  announcements from peers are received and acted on.
- [Pruned nodes](pruning.md). A pruned node never advertises `NODE_NETWORK`.
  With `state.prune_blocks_older_than` of at least 2,304, it advertises the
  draft's `NODE_NETWORK_LIMITED` service to version 2 peers instead, since
  pruning keeps at least that many recent blocks. It serves
  headers for the whole chain, and answers requests for pruned blocks with
  not-found results. A syncing node does not keep version 2 peers it dials
  unless they advertise `NODE_NETWORK`, as on the legacy protocol. The
  `getnetworkinfo` RPC reports the services sent to legacy peers.
- [State snapshots for pruned nodes](state-snapshot-sync.md) (experimental).
  A node with `sync.serve_state_snapshots` advertises the draft's
  `NODE_SYNC_ARTIFACTS` service and answers `get-object` requests for the
  manifest and pieces of the snapshots it holds, by hash, with at most two
  transfers per peer and eight in total. An empty pruned node with
  `sync.state_snapshot_sync` dials its snapshot peers over a short-lived
  version 2 connection before its network starts, downloads and verifies the
  snapshot, and then syncs the blocks above it. That connection advertises no
  services, so the serving node sees a peer that serves nothing, and it counts
  towards `max_connections_per_ip` with the node's normal connections. Both
  services are version 2 only: legacy peers are not told about
  `NODE_SYNC_ARTIFACTS`, and `getnetworkinfo` does not report it.
- The same IP bans and per-IP limits as the legacy protocol, with separate
  inbound and outbound limits of half the legacy ones. Protocol violations
  close the connection with the draft's error codes.
- Misbehavior scores as the draft describes them. A misbehaving response, such
  as a header chain that does not follow from the request, adds to the peer's
  score, which lasts across reconnections for about a ban duration. A peer
  that reaches the ban threshold is disconnected and banned, and a banned
  configured peer is not redialled until its ban lapses. The draft's penalty
  table does not list header chains that do not follow from the request yet,
  so their penalty, the same as for an over-long header chain, is Zebra's own
  choice. Headers that start after the genesis block, when Zebra's request did
  not include it, are ignored without a penalty.
- When Zebra shuts down, it closes its version 2 connections, so its peers do
  not keep a dead connection to it. Like the legacy listener, a node accepts
  one connection attempt per IP address per reconnection delay, so a restarted
  node can take two to three minutes to reconnect to a peer it connected to just
  before it stopped.

## Serving block ranges

A node with `v2_listen_addr` set answers `get-block-range` requests from its
version 2 peers. A request names a block and asks for it and its ancestors,
newest first, up to a block count and a byte limit that the requester chooses,
at most 65,536 blocks and 64 MiB. Zebra sends the whole range unless it reaches
the genesis block, or a block it does not have.

Serving is bounded so that it cannot crowd out the node's other work:

- Each peer can have two block-range streams open at once, separate from its
  other requests, and the node serves eight streams at once in total. A
  request beyond either limit is refused, and the peer asks another node.
- Zebra reads at most 2 MiB of blocks at a time for each stream, plus the
  part's first block, and keeps two buffers of up to one block each. With
  2 MB blocks that is at most about 8 MB of block data per stream, so eight
  streams hold at most about 64 MB of blocks in memory.
- A stream is cancelled if its reader does not take each 2 MiB part within
  20 seconds, if it is sent at less than 64 KiB/s on average over 20 seconds,
  or if it lasts longer than 5 minutes.

Set `network.v2_serve_block_ranges = false` to refuse every block-range
request.

## Serving known hashes

A node with `v2_listen_addr` set also answers `get-hashes` requests from its
version 2 peers. A request names a start height and a count of at most 50,000
heights. The answer has one entry per block of the best chain, from the start
height up: the block's hash, its size, and its transaction, transparent
output, and note commitment counts. A full answer is about 2 MB. A requester
must check the entries against a pinned known-hash list before it uses them,
so a node's answer is never trusted on its own.

Zebra answers with a prefix of the requested heights:

- It stops 1,000 blocks below its best tip, so it never serves an entry that a
  chain reorganization could change. A node whose chain is no longer than
  that answers with no entries.
- It stops at the first block whose entry it has not stored. The 29.1.0
  database upgrade stores the entries of blocks the node already has, from
  genesis upwards, in the background. While it runs, the node serves entries
  only up to the height the upgrade has reached, and answers requests above
  it with no entries. New blocks store their entries when they are committed.

Serving is bounded so that it cannot crowd out the node's other work:

- Each peer can have two `get-hashes` streams open at once, separate from its
  other requests and from its block-range streams, and the node serves eight
  in total. A request beyond either limit is refused, and the peer asks
  another node.
- A peer must read the answer within 5 seconds. Otherwise the stream is
  cancelled, and the node refuses that connection's `get-hashes` requests for
  60 seconds, so peers that never read their answers cannot keep the node's
  eight streams busy.

Serving `get-hashes` is always on when `v2_listen_addr` is set. Unlike block
ranges, answers are small and few, so there is no setting to turn it off.

## Serving objects

A node with `v2_listen_addr` set advertises the `NODE_SYNC_ARTIFACTS` service in
its version 2 handshake, and answers `get-object` requests from its version 2
peers. A request names an object by its SHA-256 hash, and a byte range of at
most 32 MiB. The node serves the objects in its object index: the
[spentness-hint artifact](spentness-hints.md#serving-the-artifact) for the
running release's pin, when the node has it, and, with
[`serve_state_snapshots`](state-snapshot-sync.md) on, the verified state
snapshots in its store. It answers that it does not have
any other object, and a request for no data, or for data at or past the end of
an object, with the object's size alone. The requester checks the whole
object's hash, so a node's answer is never trusted on its own.

Serving is bounded so that it cannot crowd out the node's other work:

- Each peer can have two `get-object` downloads at once, and the node serves
  eight in total. A request beyond either limit is refused, and the peer asks
  another node. Size answers and objects the node does not have take no slot.
- The node reads the object from disk 1 MiB at a time, and writes each piece
  before it reads the next.
- The node finishes every answer a few seconds before the request's deadline,
  and earlier if a peer reads a piece too slowly, so a peer that reads slowly
  gets a shorter answer and asks again for the rest.

## Checkpoint sync over version 2

With `sync.v2_checkpoint_sync = true`, Zebra downloads the part of the chain
below its final checkpoint from version 2 peers with `get-block-range`
requests, instead of asking legacy peers for one block hash list and a few
blocks at a time. It is off by default.

```toml
[network]
v2_listen_addr = "[::]:8233"

[sync]
v2_checkpoint_sync = true
```

The engine downloads from the version 2 peers Zebra connects to, configured or
discovered. With `v2_peer_discovery = false`, list at least two peers that
serve block ranges in `initial_v2_peers`: with fewer, the engine takes over
only if other version 2 peers connect to this node, which a syncing node rarely
sees, and Zebra warns at startup.

How it works:

- The checkpoint sync engine runs beside the legacy syncer, and only one of
  them downloads blocks at a time. The legacy syncer always downloads the
  genesis block first. Then, if the state tip is below the final checkpoint,
  it waits up to 5 seconds for the engine to take over before it downloads
  more blocks.
- The engine takes over when at least two version 2 peers that serve block
  ranges are connected, and the state tip is still far below the final
  checkpoint (about 20 checkpoint intervals). It uses every version 2
  connection, whichever node opened it.
- It streams whole checkpoint intervals from several peers at once, newest
  block first, and checks that each block's hash chains to the next one up to
  a checkpoint before it commits the blocks in height order. Blocks below the
  final checkpoint are checked against the checkpoint list, as the legacy
  checkpoint sync checks them.
- It hands block download back to the legacy syncer at the state tip, which is
  a checkpoint height unless a block commit failed: when it reaches its target,
  which is at most the final checkpoint, or when fewer than two of its peers
  stay connected for a minute, or when the oldest blocks it needs fail to
  download five times without progress, from five different peers or from
  every peer it can use. The legacy syncer then downloads the rest of the
  chain, including every block above the final checkpoint. If version 2 peers
  come back while the tip is still far behind, the engine takes over again a
  minute later, without using peers at the IP addresses it blamed for its
  last hand-back, even over new connections.
- While the engine downloads, the mempool and block gossip stay off, as they do
  during a legacy initial sync.

### Known-hash anchors

With a known-hash list, the engine can start and end a download at any height
below the list's end, not only at checkpoints. The list pins the SHA-256 of
each 50,000-height chunk of `get-hashes` entries. The engine fetches the
chunks it needs from its version 2 peers, a few chunks above the committed tip
at a time, and uses a chunk only once it matches its pin. A chunk that does not
match is fetched again from another peer, and the peer that sent it is not
penalized, because it may simply have a different view of the chain. The
entries give each block's exact size, so the engine asks for about 16 MiB of
blocks just above the tip and 64 MiB deeper. It also checks that each block it
downloads has the hash of its verified entry. Where a chunk is not verified
yet, the engine downloads whole checkpoint intervals as before, so a slow list
never stops it. Blocks are still committed one checkpoint interval at a time.

Zebra pins a known-hash list for Mainnet, up to height 3,522,060, and for
the default Testnet, up to height 3,466,400. Regtest and custom testnets have
no list unless they configure one with `v2_known_hash_pins`, for example with
the pins that
[`zebra-utils`' `known-hashes` tool](https://github.com/ZcashFoundation/zebra/tree/main/zebra-utils#known-hashes)
prints:

```toml
[sync.v2_known_hash_pins]
max_height = 99_999
chunk_hashes = ["<64 hex digits>", "<64 hex digits>"]
```

Zebra does not start if the pins do not cover exactly heights `0` to
`max_height`, if `max_height` is above the final checkpoint, or if they are set
for Mainnet or the default Testnet. Nodes serve entries only up to 1,000 blocks
below their tips, so a list is only fetched from peers whose chains are at
least that much longer.

The engine logs when it takes over and when it hands back:

```text
the checkpoint sync engine owns block download, waiting for it to finish target=Height(...)
the checkpoint sync engine handed block download back to the syncer state_tip=Some(Height(...))
```

Its settings are in the `[sync]` section:

| Field | Default | Meaning |
| --- | --- | --- |
| `v2_checkpoint_sync` | `false` | Turns the engine on. |
| `v2_lookahead_bytes` | `536870912` (512 MiB) | The most block data the engine requests ahead of the committed tip. More keeps more peers busy and uses more memory. Values below `2000000`, one maximum-size block, are raised to it. |
| `v2_min_peer_rate` | `262144` (256 KiB/s) | A peer that sends slower than this for 10 seconds has its request cancelled and given to another peer. It is not penalized, because a slow peer is not a misbehaving one. |
| `v2_target_peers` | `8` | The number of peers the engine downloads from at once. `0` is raised to `1`. |
| `v2_known_hash_pins` | unset | A known-hash list for Regtest or a custom testnet (see [Known-hash anchors](#known-hash-anchors)). |

The engine reports which component owns block download and how its downloads
go in these metrics, and its progress in the usual `state.*` and
`zcash.chain.*` ones:

| Metric | Type | Meaning |
| --- | --- | --- |
| `sync.v2.owner` | gauge | `1` while the engine owns block download, `0` while the legacy syncer does |
| `sync.v2.lookahead.bytes` | gauge | The block data the engine has requested or downloaded and not yet committed, counted against `v2_lookahead_bytes` (downloaded blocks count three times their size, for their size in memory) |
| `sync.v2.peer.rate` | histogram | Each downloading peer's measured rate, in bytes per second, sampled four times a second |
| `sync.v2.stalls` | counter | Requests cancelled for being too slow, by `rule`: `no_bytes` (nothing for 5 seconds), `min_rate` (below `v2_min_peer_rate` for 10 seconds) or `slow` (below a quarter of the median rate for 15 seconds) |
| `sync.v2.unit.bytes` | histogram | The bytes of each completed download unit, one or more checkpoint intervals, or a run of blocks between known hashes |

## Known limitations

- **One connection per host across both protocols.** By design, Zebra keeps
  at most one connection to each IP address and port, whatever its protocol.
  Each protocol's dialer skips a host that the other protocol is already
  connected to, so connections are not dropped and redialled. An existing
  legacy connection is not replaced with a version 2 one: while Zebra has a
  legacy connection to a version 2 peer, configured or discovered, it dials
  that peer over version 2 only after the legacy connection closes. `max_connections_per_ip`
  still applies across both protocols. Version 2 connections add up to half of
  each legacy connection limit on top of it.
- **Blocks are not announced to version 2 peers.** A version 2 block
  announcement carries the block header, which Zebra's block gossip does not
  have yet. Version 2 peers learn of new blocks from their other peers, or by
  requesting headers. Early block relay and compact blocks
  (`experimental_early_block_relay`) also use legacy connections only.
- **Snapshot peers are not discovered.** A node that bootstraps from a
  [state snapshot](state-snapshot-sync.md) downloads it only from
  `sync.state_snapshot_peers`, or from `initial_v2_peers` when that is empty.
- **Host names are resolved once.** A host name in `initial_v2_peers` that does
  not resolve at startup is retried, but once it resolves Zebra keeps using
  that address. Restart Zebra to pick up a changed address.
- **Mempool snapshots use legacy peers.** The draft's `get-mempool` request is
  an open-ended subscription, so requests for the whole mempool go to legacy
  peers only.
- **Custom testnets cannot use it.** The draft assigns ALPN identifiers only to
  Mainnet, the default Testnet, and Regtest. On a custom testnet, Zebra refuses
  to start when `v2_listen_addr` is set.
- **Other QUIC-based Zcash nodes are not the same protocol.** Nodes that speak
  a different QUIC protocol on the same port (for example Zakura's native
  transport) negotiate a different ALPN identifier, so a connection between
  them and a Zebra version 2 endpoint fails during the TLS handshake and
  neither side is affected. Those nodes still connect to Zebra over the legacy
  TCP protocol.

## Testing two nodes locally

On Regtest, two Zebra instances on one machine can talk over the version 2
protocol. Give each its own ports and state directory, and point the second at
the first:

```toml
# Node A
[network]
network = "Regtest"
listen_addr = "127.0.0.1:18344"
v2_listen_addr = "127.0.0.1:18344"
initial_v2_peers = []

# Node B
[network]
network = "Regtest"
listen_addr = "127.0.0.1:18345"
v2_listen_addr = "127.0.0.1:18345"
initial_v2_peers = ["127.0.0.1:18344"]
```

To keep such a pair closed, so it does not dial any version 2 peer it hears
of, set `v2_peer_discovery = false` on both nodes.

Node B logs `connected to initial v2 peer` at the default `info` level. Node A
logs the version 2 handshake at the `debug` level, for example with
`RUST_LOG=zebra_network=debug`. `getpeerinfo` does not list version 2 peers yet.

Give the two protocols different port numbers when both nodes run on one
machine and one of them bootstraps from a [state snapshot](state-snapshot-sync.md)
served by the other: peers are keyed by address and port across both protocols,
so a node that has a legacy connection to `127.0.0.1:18344` does not keep a
version 2 connection to the same address. The serving node also needs
`max_connections_per_ip` of at least 4: the other node's bootstrap, version 2,
and legacy connections all come from `127.0.0.1`, the limit counts the attempts
from one address within about two minutes, and a restart of the other node adds
more.

## Spentness hints

A node with an empty state can build its UTXO set below the final checkpoint
from a pinned artifact that version 2 peers serve. See
[Spentness hints](spentness-hints.md).
