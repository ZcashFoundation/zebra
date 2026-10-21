# Version 2 Peer-to-Peer Protocol (Beta)

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

## Enabling It

Set a UDP listen address in the `[network]` section of `zebrad.toml`, and list
the peers to connect to over the version 2 protocol:

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
by the `getpeerinfo` RPC yet, because that lists the address book and version 2
peers are not recorded there (see below).

## What Works

- Accepting version 2 connections, and connecting to the configured
  `initial_v2_peers`. Configured peers are reconnected after a disconnection,
  after the reconnection delay plus a random extra of up to a quarter of it. A
  configured peer that already has a version 2 connection to this node is not
  dialled.
- Serving and requesting headers, blocks, and transactions. Zebra answers
  block and transaction requests in request order, up to about 1 MB per
  response.
- Exchanging peer addresses as the draft recommends: Zebra asks for addresses
  once, on connections it made, and answers once, on connections it accepted.
- Transaction announcements in both directions. Block and address
  announcements from peers are received and acted on.
- The same IP bans and per-IP limits as the legacy protocol, with separate
  inbound and outbound limits of half the legacy ones. Protocol violations
  close the connection with the draft's error codes.
- When Zebra shuts down, it closes its version 2 connections, so its peers do
  not keep a dead connection to it. Like the legacy listener, a node accepts
  one connection attempt per IP address per reconnection delay, so a restarted
  node can take two to three minutes to reconnect to a peer it connected to just
  before it stopped.

## Known Limitations

- **Peers are not discovered.** Zebra has one address book for both
  protocols, and it does not record which protocol an address was learned
  over. So Zebra does not record the version 2 peers it connects to, and
  connects over version 2 only to the hosts in `initial_v2_peers`. Addresses
  that version 2 peers send still join that address book, where they are
  treated as legacy TCP addresses. Zebra answers version 2 address requests
  from the same address book, so some of the addresses it sends may not accept
  QUIC.
- **One connection per host across both protocols.** Peers are keyed by IP
  address and port whatever their protocol, and `max_connections_per_ip`
  applies across both. While Zebra has a legacy connection to a host in
  `initial_v2_peers`, the version 2 connection to it is dropped and redialled
  after the reconnection delay. While it has a version 2 connection to such a
  host, its legacy crawler may still dial the same address over TCP, and that
  connection is dropped. Version 2 connections add up to half of each legacy
  connection limit on top of it.
- **Blocks and addresses are not announced to version 2 peers.** A version 2
  block announcement carries the block header, which Zebra's block gossip does
  not have yet, and address relay is not implemented. Version 2 peers learn of
  new blocks from their other peers, or by requesting headers.
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

## Testing Two Nodes Locally

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

Node B logs `connected to initial v2 peer` at the default `info` level. Node A
logs the version 2 handshake at the `debug` level, for example with
`RUST_LOG=zebra_network=debug`. `getpeerinfo` does not list version 2 peers yet.
