# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added `zebra_network::zakura`, a default-off iroh scaffold that exposes a
  relay/discovery-off endpoint builder and reserves the persistent Zakura iroh
  node secret-key path and config field.
- Added `PeerServices::NODE_P2P_V2`, the default-on `v2_p2p` and `legacy_p2p`
  network configs, and a neutral legacy-handshake upgrade hook for mutually
  capable Zakura peers.
- Added bounded Zakura P2P v2 upgrade prelude and control-handshake wire types,
  including transcript binding, native-vs-upgraded control validation, and
  duplicate-peer handling scaffolding.
- Added the default-off Zakura iroh protocol handler, explicit QUIC transport
  limits, native bootstrap peer config, and bounded admission/stream/message
  limit enforcement.
- Added the `zakura-testkit` feature with deterministic loopback Iroh endpoint
  tooling, in-process Zakura node/cluster harnesses, a bounded inbound recorder,
  and raw hostile-peer helpers for protocol tests.
- Wired the legacy-gossip adapter into the running node: when `v2_p2p` is
  enabled, `init` installs `LegacyGossipSink` on the Zakura endpoint (replacing
  the drop sink) and wraps the returned peer set in `ZakuraDualStackService`, so
  locally originated gossip and inventory fetches fan out across both the legacy
  TCP peer set and Zakura. A v2-capable node now coexists with legacy-only peers
  (legacy traffic over TCP, mutually capable peers also gossip over Zakura).
- Implemented the legacy->Zakura upgrade: after a mutually `NODE_P2P_V2`-capable
  legacy `version`/`verack`, the peers exchange a bounded `P2pV2Upgrade` prelude
  over the legacy TCP stream to learn each other's iroh node address, and the
  TCP initiator dials the responder over QUIC. The connection is then registered
  with the supervisor (incrementing `zakura.p2p.handshake.upgraded`) and the
  legacy stream is dropped, so mutually capable peers move their gossip and
  inventory traffic onto Zakura with no configured bootstrap peers. Any neutral
  problem (no live endpoint, malformed/rejected prelude) falls back to legacy.
- Added Zakura header-sync stream-5 wire constants, bounded message codecs,
  stateless header validation, and the default `network.zakura.header_sync`
  config surface.

### Changed

- `zakura::spawn_zakura_endpoint` now takes an inbound-sink factory
  (`impl FnOnce(ZakuraSupervisorHandle) -> Arc<dyn InboundSink>`) so callers can
  install a sink backed by the endpoint's supervisor. Pass a factory returning
  `Arc::new(DropInboundSink)` to keep the previous drop-everything behavior.
- `ZakuraHandshakeConnector` is now backed by the live `ZakuraEndpoint` (it reads
  the local dial hints and dials the peer over QUIC) rather than carrying the
  placeholder `ZakuraUpgradeRequest`/`upgrade_outcome` hook, which has been
  removed now that the upgrade is implemented.
- `ZakuraDualStackService` now routes every request the Zakura adapter can serve
  — chain-sync discovery (`FindBlocks`/`FindHeaders`) and mempool data
  (`MempoolTransactionIds`/`PushTransaction`), in addition to the existing
  inventory fetches — through the legacy-first-then-Zakura fallback path. These
  were previously passed through to the legacy peer set only, so a node whose
  only peer was upgraded to Zakura could never obtain tips, fetch blocks, or push
  transactions (its syncer requests timed out against the empty legacy peer set).
- `ZakuraDualStackService` inventory fetches now bound the legacy attempt before
  falling back to Zakura, so a node whose only peer is over Zakura (its legacy
  peers were upgraded) no longer blocks every fetch on the empty legacy peer set.
- Raised `DEFAULT_ZAKURA_QUIC_IDLE_TIMEOUT` from 30s to 150s. The 30s
  application-idle reaper tore down healthy gossip connections between blocks
  (which can be minutes apart) and forced constant re-dials.

### Fixed

- A peer upgraded from legacy TCP to Zakura is no longer re-dialed over legacy.
  The upgrade drops the legacy connection, so nothing refreshed the peer's
  `Responded` liveness; once it aged past `MIN_PEER_RECONNECTION_DELAY` the
  outbound crawler reconnected to it, re-running the upgrade and churning the
  QUIC connection. The handshake now keeps the upgraded peer's address-book
  entry live (mirroring the legacy heartbeat) for as long as the Zakura
  connection is registered with the supervisor, and stops once it deregisters so
  a genuinely gone peer is reconnected normally.

## [8.0.0] - 2026-06-02

### Changed

- Require network protocol version 170150 for NU6.2 on Mainnet, Testnet, and Regtest.
- Bump `CURRENT_NETWORK_PROTOCOL_VERSION` to 170150.

## [7.0.0] - 2026-05-28

This release fixes three network security issues:

- Cap pre-handshake message body length in `Codec` to `MAX_HANDSHAKE_BODY_LEN`
  (1 KB); the limit is raised to `MAX_PROTOCOL_MESSAGE_LEN` after the
  handshake completes
  ([GHSA-h72h-ppcx-998p](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-h72h-ppcx-998p)).
- Tag transaction-advertisement requests with the announcing peer so the
  mempool can enforce a per-peer queue cap
  ([GHSA-4fc2-h7jh-287c](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-4fc2-h7jh-287c)).
- Canonicalize IPv4-mapped addresses on the misbehavior path so a peer cannot
  evade scoring by alternating between `IPv4` and `IPv4-mapped-IPv6` forms of
  the same address
  ([GHSA-63wg-wjjj-7cp8](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-63wg-wjjj-7cp8)).

The impact of these issues for crate users will depend on the particular
usage; if you use it as a building block for a consensus node, you should
update.

### Added

- `MetaAddr::new_misbehavior(addr: PeerSocketAddr, score_increment: u32) -> MetaAddrChange`,
  which canonicalizes IPv4-mapped addresses before scoring.
- `Codec::reconfigure_full_body_len(&mut self)`, raising the codec's body
  limit from the pre-handshake cap (`MAX_HANDSHAKE_BODY_LEN = 1024`) to
  `MAX_PROTOCOL_MESSAGE_LEN` after handshake completion.

### Changed

- `Request::AdvertiseTransactionIds` is now a 2-tuple variant:
  `AdvertiseTransactionIds(HashSet<UnminedTxId>, Option<PeerSocketAddr>)`.
  The new second field carries the announcing peer for per-peer queue caps.
  Affects `Display`, `Request::command`, and all pattern matches.
- `Codec` default builder now starts with `max_len = MAX_HANDSHAKE_BODY_LEN`;
  pre-handshake messages above 1 KB are rejected.
- Network config: `testnet_parameters` can now be supplied either via the
  legacy `testnet_parameters` table or via an untagged `DNetwork` enum
  (`network = "..."` plus inline params). Serialization emits the new form;
  the legacy form remains deserializable
  ([#10051](https://github.com/ZcashFoundation/zebra/pull/10051)).
- `zebra-chain` dependency bumped to `8.0.0`.

### Fixed

- `AddressBook` no longer panics on the ban path when
  `max_connections_per_ip != 1`; the optional `most_recent_by_ip` cache is
  now guarded instead of unwrapped
  ([#10580](https://github.com/ZcashFoundation/zebra/issues/10580)).

## [6.0.0] - 2026-05-01

This release adds defense in depth for inbound deserializers. The
`zebra-chain` 7.0 cohort enforces 160-entry cap in `read_headers` and
size-limits coinbase data and Equihash solutions before allocation
([GHSA-438q-jx8f-cccv](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-438q-jx8f-cccv)).

### Changed

- `Request::AdvertiseBlock` now carries a second tuple field
  `Option<PeerSocketAddr>` so the inbound service can attribute the announcing
  peer when fanning out.

## [5.0.1] - 2026-04-17

This release fixes an important security issue:

- [CVE-2026-40881: addr/addrv2 Deserialization Resource Exhaustion](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-xr93-pcq3-pxf8)

The impact of the issue for crate users will depend on the particular usage; if
your application allows deserializing arbitrary `addr` and/or `addrv2` messages,
you should update.

## [5.0.0] - 2026-03-12

### Breaking Changes

- `zebra-chain` dependency bumped to `6.0.0`.

### Added

- `PeerSocketAddr` now derives `schemars::JsonSchema`

## [4.0.0] - 2026-02-05

### Breaking Changes

- `zebra-chain` dependency bumped to `5.0.0`.

## [3.0.0] - 2026-01-21 - Yanked

### Breaking Changes

- Added `rtt` argument to `MetaAddr::new_responded(addr, rtt)`

### Added

- Added `MetaAddr::new_ping_sent(addr, ping_sent_at)` - creates change with ping timestamp
- Added `MetaAddr::ping_sent_at()` - returns optional ping sent timestamp
- Added `MetaAddr::rtt()` - returns optional round-trip time duration
- Added `Response::Pong(Duration)` - response variant with duration payload

## [2.0.2] - 2025-11-28

No API changes; internal dependencies updated.

## [2.0.1] - 2025-11-17

No API changes; internal dependencies updated.

## [2.0.0] - 2025-10-15

Added a new `Request::AdvertiseBlockToAll` variant to support block advertisement
across peers ([#9907](https://github.com/ZcashFoundation/zebra/pull/9907)).

### Breaking Changes

- Added `AdvertiseBlockToAll` variant to the `Request` enum.

## [1.1.0] - 2025-08-07

Support for NU6.1 testnet activation.

### Added

- Added support for a new config field, `funding_streams`
- Added deserialization logic to call `extend_funding_streams()` when the flag is true for both configured Testnets and Regtest

### Deprecated

- The `pre_nu6_funding_streams` and `post_nu6_funding_streams` config
  fields are now deprecated; use `funding_streams` instead.

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
