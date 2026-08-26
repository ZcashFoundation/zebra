# v2 P2P Protocol — Spec Conformance Tracking

Implemented against the draft ZIPs in zcash/zips PR [#1346] at revision
**`a3f4fa2a`** ("Harden misbehavior tracking against induced-ban and
misattribution attacks", 2026-08-06), which includes the PR [#1344] transport
draft. The drafts are unreviewed past daira's 2026-08-04 pass on #1344, so
sections added after that pass are expected to move; on every draft change,
update the pinned revision here and re-check the affected rows.

[#1344]: https://github.com/zcash/zips/pull/1344
[#1346]: https://github.com/zcash/zips/pull/1346

Status legend: ✓/● implemented · ◐ partial · ✗ absent. Gap numbers (G1–G23)
and phase numbers refer to the implementation plan.

The remaining ◐ and ✗ entries are deferred for recorded reasons:
client-side synchronization scheduling converges with the ibd-engine work;
the Tor transport is blocked on an `arti` dependency conflict; per-address
transport hints need a draft change (feedback item 3); and
`MIN_V2_PROTOCOL_VERSION` needs a network upgrade assignment.

| Draft section | Implementing module | Tests | Status |
|---|---|---|---|
| Network Parameters / Networks (ALPN) | `protocol/v2/quic.rs`, `protocol/v2/constants.rs` | `protocol/v2/quic/tests/vectors.rs` | ◐ custom-testnet ALPN unresolved (G15, spec feedback) |
| DNS Seeds | — (external seeder work) | — | ✗ deployment (Phase 7) |
| Peer Discovery | `peer_set/initialize.rs`, `peer_book/transports.rs` | `peer_book/transports/tests.rs` | ◐ the crawler dials whichever transport a peer is known to accept, and probes unknown peers, because addresses carry no transport hint (feedback item 3); learned reachability is not persisted across restarts (G12) |
| Transports / Connection Management | `peer_set/initialize/{v2_transport,inbound_admission}.rs`, `peer/v2/connector.rs`, `peer_set/limit.rs` | `peer_set/initialize/tests/vectors.rs` | ◐ both transports share one admission path, one connection counter and one connector interface; the shared limits are tested through the v1 listener, the v2 dial policy is not yet tested |
| QUIC Transport | `protocol/v2/quic.rs`, `peer_set/initialize/v2_transport.rs` | `protocol/v2/quic/tests/vectors.rs` | ● retry-token address validation required above 32 concurrently pending inbound handshakes (G7; stateless retry path is quinn-provided, not separately tested); UDP buffer/GSO tuning left to quinn defaults |
| QUIC / Certificates | `protocol/v2/quic.rs` | `protocol/v2/quic/tests/vectors.rs` | ◐ presents X.509/Ed25519; accepts X.509 with Ed25519 + P-256; RFC 7250 raw public keys blocked on rustls (G6, spec feedback item 2) |
| QUIC / Bulk Transfer Performance | `protocol/v2/quic.rs` | — | ✗ bulk flow-control profile (G16, Phase 5) |
| QUIC / Stream Layer Mapping | `protocol/v2/quic.rs` | `peer/v2/service/tests/vectors.rs` | ✓ |
| Tor Transport | — | — | ✗ blocked on the `arti` x25519-dalek version conflict. A sans-IO framing layer was written and then removed as unreachable code; recover it from git history when `arti` unblocks |
| Stream Layer / Transport Requirements | `protocol/v2/quic.rs` | `protocol/v2/quic/tests/vectors.rs` | ✓ stream limits, no datagrams, 0-RTT never negotiable (G20) |
| Stream Types | `protocol/v2/types.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ✓ `0x00`–`0x09`, `0x10`–`0x12`; unknown types refused without penalty, both directions |
| Request Streams | `peer/v2/connection.rs` | `peer/v2/service/tests/vectors.rs` | ✓ incl. stalled-stream abandonment after `INBOUND_STREAM_TIMEOUT` |
| Announcement Streams | `peer/v2/connection.rs` | `peer/v2/service/tests/vectors.rs` | ✓ singleton rule + reopen-after-reset tested; own-listener announcements sent on every connection (G9) |
| Records | `protocol/v2/record.rs` | `protocol/v2/tests/vectors.rs` | ✓ |
| Application Error Codes | `protocol/v2/types.rs` | `protocol/v2/tests/vectors.rs` | ✓ |
| CompactSize | `protocol/v2/record.rs` (via `zebra-chain` `CompactSize64`) | `protocol/v2/tests/vectors.rs` | ✓ canonical both directions |
| Strings | `protocol/v2/init.rs` | `protocol/v2/tests/vectors.rs` | ✓ |
| Serialized Blocks | `zebra-chain` serialization | `protocol/v2/tests/vectors.rs` | ✓ |
| Network Address Record | `protocol/external/addr/v2.rs` | `protocol/external/addr` tests | ✓ |
| Transaction References | `protocol/v2/txref.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ✓ incl. wrong-typed `get-tx` refs answered not-found, no penalty |
| Service Flags | `protocol/external/types.rs`, `peer_set/initialize.rs` | — | ● bits 3/4/10 defined; `NODE_TREE_ROOTS` advertised (index backfilled by the state upgrade; pre-backfill requests refused); `NODE_SYNC_ARTIFACTS` advertised when the artifact directory exists or the network pins known-hash chunks (served from state); `NODE_NETWORK_LIMITED` unused (full nodes advertise `NODE_NETWORK`) |
| Connection Handshake | `protocol/v2/init.rs`, `peer/v2/handshake.rs` | `peer/v2/handshake/tests/vectors.rs` | ✓ |
| Protocol Versioning | `protocol/v2/constants.rs` (`MIN_V2_PROTOCOL_VERSION` placeholder), `peer_set/set.rs` | — | ◐ NU assignment pending; mid-connection epoch enforcement shared with v1 (G14): `disconnect_from_outdated_peers` reads the v2 init version via `RemoteHandshake`, and handshakes enforce `max(MIN_V2, MinimumPeerVersion)` |
| `get-headers` | `protocol/v2/{request,response}.rs`, `peer/v2/connection.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ● served incl. `tx_ids` (coinbase + full IDs; unavailable blocks answer `has_txs = 0`); requester side sends `tx_ids = 0` until Phase 5 |
| `get-blocks` | `protocol/v2/{request,response}.rs`, `peer/v2/connection.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ✓ hash-only requests, full-block results (G1) |
| `get-tx` | `protocol/v2/{request,response,txref}.rs`, `peer/v2/connection.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ✓ |
| `get-addr` | `protocol/v2/{request,response}.rs`, `peer/v2/connection.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ✓ direction policy + at most one request per connection (G8) |
| `get-mempool` | `protocol/v2/{request,response}.rs`, `peer/v2/connection.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ● subscription: snapshot records then per-batch updates (lag → re-snapshot; duplicates allowed, not deduped); single-concurrent enforced (PROTOCOL_ERROR), sequential re-subscribe served, prompt cancel detection; requester keeps one subscription per connection, mirrors into a 50k-bounded cache answering internal mempool requests |
| `get-hashes` | `protocol/v2/{request,response}.rs`, `peer/v2/connection.rs`, `zebra-state` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ● served from the state sync-metadata index: best-chain hashes + span sums, tip margin of `MAX_BLOCK_REORG_HEIGHT` (stricter than the draft's 100), prefix truncation (also at the backfill frontier); requester (internal `Request::RemoteSyncHashes`) round-trips the wire request/response, entries unverified at this layer; sync-engine scheduling converges with ibd-engine (Phase 5) |
| `get-block-range` | `protocol/v2/request.rs`, `peer/v2/connection.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ● serve: descending parent-walk streaming, exact `count`/`max_bytes` bounds with the first-block rule, not-found result, early-finish truncation, ≤2 concurrent bulk streams per peer (REFUSED beyond); request (internal `Request::BlockRange`): on-arrival anchor/parent-link/merkle verification (violations → PROTOCOL_ERROR), bound overruns → FLOOD, truncation-resumable; scheduler integration converges with ibd-engine |
| `get-tree-roots` | `protocol/v2/{request,response}.rs`, `peer/v2/connection.rs`, `zebra-state` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ● served: anchor-at-height membership check (REFUSED on mismatch or missing index), per-height sapling/orchard/ironwood roots (zeros pre-activation), ZIP 221 counts + `auth_data_root` from the index; requester (internal `Request::RemoteTreeRoots`) round-trips the wire request/response, entry verification stays with the caller (Phase 5) |
| `get-object` | `protocol/v2/request.rs`, `peer/v2/connection.rs`, `config/cache_dir.rs`, `zebra-state` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ● served: the state-backed pinned lookup (inbound `LocalObject` → zebra-state `ReadRequest::SyncArtifact`) answers pinned known-hash chunks (stored, else regenerated from state) and the pinned spentness-hint artifact before the cache-dir artifact directory (hex-named content-addressed files): exact `offset`/`length`/size bounds, size-only answers past the end, not-found for unheld objects — now also without the directory (previously `REFUSED`; `NODE_SYNC_ARTIFACTS` is advertised with the directory or when the network pins known-hash chunks); non-pinned artifact population is operator/ibd-engine work; requester (internal `Request::Object`): ranged reads, not-found maps to a zero total size, bytes verified by the caller — drives zebrad's spentness-hint fetch task |
| Block Announcements | `peer/v2/connection.rs` | `peer/v2/service/tests/vectors.rs` | ● send: compact to `announce = 1` peers (`full_ids` honored), header otherwise + oversize substitute; receive: both kinds, kind 0x01 only when locally requested, context-free header PoW checked (invalid → +100); announced-block limit is 1 (no intermediate-header backfill) |
| Transaction Announcements | `peer/v2/connection.rs` | `peer/v2/service/tests/vectors.rs` | ✓ send + receive (trickling G11, Phase 3) |
| Address Announcements | `peer/v2/connection.rs` | `peer/v2/service/tests/vectors.rs` | ✓ send (own listener, daily) + receive (G9) |
| Divided Block Relay | `peer/v2/connection.rs` | `peer/v2/service/tests/vectors.rs` | ◐ `tx_ids` serving done; requester-side reconstruction with Phase 5 (G19) |
| Compact Block Relay | `protocol/v2/compact_block.rs`, `peer/v2/connection.rs`, `peer/v2/service.rs` | `protocol/v2/tests/vectors.rs`, `peer/v2/service/tests/vectors.rs` | ● HB send + sent-block obligations (SHORTID / full-ref `get-tx`, 8-block/peer bound); HB request on ≤3 self-initiated connections (slot claim, reconnect-only rotation — most-recent-announcer preference simplified); reconstruction: mempool match → `SHORTID`/full-ref `get-tx` → merkle check → `get-blocks` fallback, no penalty; reconstructed blocks answer `BlocksByHash` from a 4-block cache; HB exemption via withheld advertiser (header kind included); LB fetches full blocks (`tx_ids` reconstruction near tip is a MAY, not used) |
| Headers-First Synchronization | `peer/v2/connection.rs` (`FindBlocks` bridge) | `peer/v2/service/tests/vectors.rs` | ◐ bridged onto `get-headers` (conforming: v2 wire is headers-first); syncer swap + bridge retirement converge with the ibd-engine scheduler (G19) |
| Checkpointed Synchronization | — | — | ✗ Phase 5 (converges with ibd-engine) |
| Block Download Parameters | — | — | ✗ Phase 5 |
| Transaction Relay / Trickling | `peer/v2/connection.rs` | `peer/v2/service/tests/vectors.rs` | ◐ per-connection exponential trickle (mean 5s, 4× cap) batching announcements + subscription updates; blocks untrickled per spec; expiring-soon (TX_EXPIRING_SOON_THRESHOLD) filter not implemented (matches v1 zebrad) |
| Transaction Expiry / Mempool Policy (penalty exemptions) | `zebrad` syncer/mempool misbehavior senders | — | ◐ audited under G21 (P1.3) |
| Address Relay / Rate Limiting / Address Book Management / Broadcasting | `peer_book/{actor,buckets,misbehavior}.rs`, `peer/v2/connection.rs` | `peer_book/buckets/tests.rs`, `peer/v2/service/tests/vectors.rs` | ◐ single-owner peer book actor (G8); own-listener announcements (G9); gossiped intake bucketed by secret-keyed /16 (v4) / /32 (v6) group, 256×32, keyed replacement (Address Book Management); per-connection token bucket 0.1/s burst 1000 on v2 announcements (Rate Limiting); misbehavior/ban store keyed by addr / v6-64 (G22/G23); remaining: multi-transport `PeerAddr` keying, persisted reachability, tried-table test-before-evict (G10/G12) |
| Misbehavior and Banning | `protocol/v2/types.rs` (`WireError::Misbehavior`), `peer/v2/connection.rs`, shared ban mechanism | `peer/v2/service/tests/vectors.rs` | ◐ provable penalties (G21) + address-keyed persistent scores with IPv6 /64 keying and bounded bans (G22, G23); whitelist exemption pending a whitelist config |
| Deployment | `config.rs` (`v2_listen`, `initial_v2_peers`) | `zebrad` config tests | ◐ config-gated; rollout Phase 7 |

## Spec feedback found during implementation

Items to file against the drafts, discovered while implementing (beyond the
feedback list already prepared with the implementation plan):

1. `get-hashes` has an explicit MUST that the greatest requested height not
   exceed `0xFFFFFFFF`, but `get-tree-roots` — whose highest requested
   height `start_height + count − 1` can also overflow — has no matching
   rule. An overflowing anchor height cannot exist, so this implementation
   accepts the request and lets the `final_hash` membership check refuse it,
   but an explicit rule (or a note that refusal is expected) would make
   implementations agree.

2. "A node MUST be prepared to accept both certificate encodings (X.509 and
   raw public key)" is not implementable with rustls (0.23): its RFC 7250
   support is all-or-nothing per endpoint — a client either offers only
   `RawPublicKey` in `server_certificate_type` (and stops interoperating
   with X.509-only peers) or does not offer the extension at all. Offering
   both types and accepting the peer's choice has no API. Consider
   downgrading RPK acceptance to SHOULD, or adding a transition note
   ("X.509 required, raw public keys optional") until TLS implementations
   can offer both.

3. Network address records carry no indication of which transports a peer
   is reachable on: `ADDRV2` IPv4/IPv6 entries cover TCP (legacy) and QUIC
   (v2) implicitly, and the Deployment section's shared-port note means a
   crawler can only *probe* UDP to discover v2 reachability before
   activation. A per-address transport hint (a service bit, or an addrv2
   network ID) would let crawlers dial v2 endpoints directly instead of
   probing; until then, this implementation probes a small fraction of
   outbound dials, and relies on DNS seeders probing QUIC as the draft
   anticipates.
