# Dual-Protocol Networking: Running v2 on the Legacy Network

Zebra speaks two peer protocols: the legacy protocol over TCP, and the draft
version 2 protocol over QUIC ([zcash/zips#1344], [zcash/zips#1346]). This
describes how they coexist, and what is left to do.

The version 2 draft has **no compatibility bridge** by design: it is a
distinct protocol deployed through the network upgrade mechanism, with epoch
enforcement retiring legacy peers at activation. Until then both run
together — QUIC uses UDP where the legacy protocol uses TCP, so both are
served on the same address and port.

[zcash/zips#1344]: https://github.com/zcash/zips/pull/1344
[zcash/zips#1346]: https://github.com/zcash/zips/pull/1346

## How they share one node

Almost everything is shared, by construction:

| Concern | Shared mechanism |
| --- | --- |
| Peer set membership | Both transports produce a `peer::Client`, delivered over one channel |
| Outbound dialing | Both are `Service<OutboundConnectorRequest>`: `peer::Connector` and `peer::V2Connector` |
| Inbound admission | One screening path, one connection counter, one per-IP limit |
| Address book | One peer book actor; legacy `addr` messages and v2 address announcements use the same intake |
| Bans | One store, keyed by IP — transport is not an identity, so a peer banned over one cannot return over the other |
| Inventory | One registry, so gossip routing already mixes transports |
| Block sync | The syncer's `FindBlocks`/`BlocksByHash` work unchanged: v2 bridges `FindBlocks` onto `get-headers` |

Two things are transport-specific. `peer_book::transports` records which
transports each address is known to accept, since relayed addresses carry no
transport hint; and `Request::peer_capability` marks requests that only
version 2 peers can answer, so the peer set does not route them to a legacy
peer. Both are documented at their definitions.

## What is left

**Learned reachability is not persisted, and is learned only from outbound
dials.** It lives in a side table beside the address book rather than on
`MetaAddr`, so an inbound v2 handshake teaches the node nothing, a restart
discards everything probing learned, and reachability cannot influence which
candidates the address book selects — only decorate them afterwards. Moving
it onto the address record fixes all three, and is what Tor addressing will
need anyway.

**Probing is how version 2 peers are discovered at all.** Because addresses
carry no transport hint, a peer not known to accept QUIC is probed at a small
rate before its legacy dial. A per-address transport hint in the draft — a
service bit, or an addrv2 network ID — would replace probing with direct
dialing; this is filed as feedback item 3 in the conformance doc.

**No preference target.** The dial policy uses version 2 wherever a peer is
known to accept it, but does not aim for a minimum number of version 2
connections. That becomes worth adding once the version 2 population is
non-trivial.

**The peer set filters capabilities on one routing path.** `route_p2c`
consults `peer_capability`; the inventory and broadcast paths do not. No
capability-bearing request reaches them today, but nothing enforces that.

At the network upgrade, epoch enforcement retires legacy peers and the dial
policy becomes version 2 only. Until then, version 2 is used opportunistically
wherever it is found.

## Two properties to preserve

**Transport is not an identity.** Bans are keyed by IP, so a peer cannot evade
one by switching transports. For the same reason, learned reachability belongs
on the address record rather than in a second address book: a peer that could
occupy two entries by being reachable two ways would weaken the address book's
eclipse resistance.

**Probes are not an amplifier.** A probe is one QUIC initial packet to an
address that already passed the book's filters — routable, correct port, not
banned — in place of the TCP SYN the node would have sent anyway.

## Configuration

`network.v2_listen` serves version 2 on the UDP port of `listen_addr`;
`network.initial_v2_peers` names version 2 peers to dial at startup, which
are seeded as QUIC-reachable so the crawler maintains them like any other
peer. Both default off, and with them off nothing about the node changes.
