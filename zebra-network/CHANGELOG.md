# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org).

## [14.0.0] - 2026-10-01

### Breaking Changes

- Explicitly empty funding-stream lists disable defaults on configured Testnets and survive configuration round-trips, as they already did on Regtest. Testnet and Regtest reject combining an empty list with either legacy funding field; remove both legacy fields to disable streams, or omit the empty list to retain legacy payouts. Omitted settings retain network defaults, and legacy post-NU6 streams use post-NU6 defaults independently of the pre-NU6 field. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- Incompatible configured Testnets reject public Testnet magic even when public seeds are omitted. Public seed matching now ignores hostname case, trailing dots, and ports. Configure distinct `network_magic` and non-public peers for incompatible consensus rules. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- `config::CacheDir::peer_cache_file_path` includes configured Testnet wire magic in peer cache names. Existing unsuffixed custom-network caches are not reused; public-network and Regtest paths are unchanged. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- Configured Testnets using public Testnet magic inherit the historical public NSM seed when omitted. Explicitly matching that seed leaves network identity and state paths unchanged. A different seed requires distinct `network_magic` and non-public peers; Regtest and distinct-magic networks default to zero ([#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).
- `Config` deserialization rejects custom Testnet subsidy schedules that exceed the monetary cap, including early spacing upgrades with the default slow-start interval. Set `slow_start_interval = 0` for accelerated test networks. ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529))
- `Config` deserialization rejects overlapping nonempty funding-stream height ranges on Testnet and Regtest, including overlaps introduced by inherited NU7 defaults. Use disjoint ranges; adjacent and empty ranges are accepted, including empty recipient lists with automatic address extension ([#11554](https://github.com/ZcashFoundation/zebra/pull/11554)).
- `Config` deserialization rejects TEX funding-stream recipients on Testnet and Regtest before node startup. Configure P2SH or P2PKH recipients instead; deferred recipients do not require addresses ([#11554](https://github.com/ZcashFoundation/zebra/pull/11554)).

### Added

- `initial_nsm_value_balance` and `nsm_reissuance_height` under `[network.testnet_parameters]` are available for Regtest and configured Testnets. Serialization preserves omitted settings. The effective reissuance height follows ZIP 237's scheduled-issuance crossover, with explicit overrides for accelerated testing ([#11454](https://github.com/ZcashFoundation/zebra/pull/11454), [#11530](https://github.com/ZcashFoundation/zebra/pull/11530)).

### Changed

- [ZIP 259](https://zips.z.cash/zip-0259) NU7 peer-version checks require 170180 on Testnet and Regtest and 170190 on Mainnet. `CURRENT_NETWORK_PROTOCOL_VERSION` is now 170180, up from 170160, until a Mainnet activation height is scheduled. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))
- `Config` rejects explicitly out-of-order upgrade heights even when coincident activations previously hid the invalid ordering ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527)).
- Serializing and deserializing a Regtest `Config` preserves the effective activation heights of coincident upgrades instead of applying earlier defaults ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527)).
- Peer-service requirements and stall detection use the local tip timestamp for freshness, preserving the elapsed-time allowance when NU7 shortens block spacing ([#11529](https://github.com/ZcashFoundation/zebra/pull/11529)).
- Regtest skips public DNS seeders before resolving initial peers, while retaining explicitly configured local peers. ([#11527](https://github.com/ZcashFoundation/zebra/pull/11527))

## [13.0.0] - 2026-09-23

### Breaking Changes

- `zebra-chain`'s `Transaction` type is now a newtype over `zcash_primitives::transaction::Transaction`, and appears in public protocol messages ([#10461](https://github.com/ZcashFoundation/zebra/pull/10461)).
- Removed the `misbehavior_score` field and `MetaAddr::misbehavior` method. Misbehavior is now tracked per peer group by the `AddressBook`, and read with the new `AddressBook::misbehavior_score` method, also available on the `AddressBookPeers` trait: currently it returns `MAX_PEER_MISBEHAVIOR_SCORE` for a banned group and `0` otherwise, because scores are not currently accumulated since those are the only two scores being used. `AddressBook::bans` now returns the new `BanList` type instead of an `Arc<IndexMap<IpAddr, Instant>>`; query it with `BanList::is_banned`, which applies both the peer group mapping and ban expiry ([#11255](https://github.com/ZcashFoundation/zebra/issues/11255)).

### Added

- A `fuzzing` feature, off by default, which makes the `protocol` module public for the coverage-guided fuzz harnesses in `zebra-fuzz/`. It activates no dependencies and leaves default and release builds unchanged ([#11221](https://github.com/ZcashFoundation/zebra/pull/11221)).

### Changed

- Concurrent reconnection-candidate selection can no longer hand the same peer to two connection attempts: a candidate is now chosen and marked `AttemptPending` in a single atomic step. `init()`, the public API, and network-visible behavior are unchanged ([#1976](https://github.com/ZcashFoundation/zebra/issues/1976)).
- Outbound connection pacing is now applied only when an address-book candidate is returned. An empty candidate-selection attempt no longer delays the next available connection attempt. Crawls are still skipped while rate-limited, and the intervals are unchanged ([#1976](https://github.com/ZcashFoundation/zebra/issues/1976)).
- `zebra-network` no longer pulls the unmaintained `ordered-map` crate or its legacy `quickcheck` 0.9 and `rand` 0.7 dependency subtree into downstream builds ([#10516](https://github.com/ZcashFoundation/zebra/issues/10516)).
- `network.max_connections_per_ip` now limits IPv6 peer connections per `/64` subnet, rather than per individual address. A single machine with a standard IPv6 `/64` allocation has 2^64 distinct addresses, so per-address limiting did not bound the number of connections one machine could open. IPv4 connections are still limited per address ([#11255](https://github.com/ZcashFoundation/zebra/issues/11255)).
- Peer bans now expire after 24 hours, matching `zcashd`'s `DEFAULT_MISBEHAVING_BANTIME`. Bans were previously kept until restart. A peer whose ban has lapsed is banned again as soon as it misbehaves again. Lapsed bans are pruned when a new ban is applied ([#11255](https://github.com/ZcashFoundation/zebra/issues/11255)).

### Security

- Peer misbehavior bans now apply to the whole peer group — one IPv4 address, or one IPv6 `/64` subnet — instead of a single address. Bans were previously keyed by the full address, so a peer could avoid its ban by reconnecting from another of the 2^64 addresses in its `/64` ([#11255](https://github.com/ZcashFoundation/zebra/issues/11255)).

## [12.0.0] - 2026-08-10

### Breaking Changes

- Requires `zebra-chain` 12.0.0, whose block and transaction types appear in public protocol
  request, response, and inventory APIs.

### Added

- `seeder.zec.rocks` and `seeder.testnet.zec.rocks` are now default DNS seeders in
  `Config::default()`, for Mainnet and Testnet respectively
  ([#11096](https://github.com/ZcashFoundation/zebra/pull/11096)).
- Connection-attempt, terminal-outcome, and remote-version metrics with bounded network,
  direction, address-family, lifecycle-stage, outcome, and implementation labels
  ([#11135](https://github.com/ZcashFoundation/zebra/pull/11135)).
- `constants::MISBEHAVIOR_FLUSH_INTERVAL`, the interval between flushes of batched peer
  misbehaviour updates into the address book. This was previously an unnamed literal in
  `init_with_block_gossip_peer_ips()`; the value is unchanged outside this crate's tests
  ([#11129](https://github.com/ZcashFoundation/zebra/pull/11129)).

### Changed

- Peer-set, crawler-handshake, and address-book gauges now include a `network` label, so multiple
  network instances in one process do not overwrite each other's values
  ([#11135](https://github.com/ZcashFoundation/zebra/pull/11135)).
- `AddressBook::update()` logs a change rejected for a banned peer IP at `debug` instead of `warn`,
  since remote peers control how often it fires
  ([#11173](https://github.com/ZcashFoundation/zebra/pull/11173)).

### Fixed

- Banning a peer IP now removes every address book entry for that IP, and `reconnection_peers()`
  never returns an address whose IP is banned. Previously an entry for the banned IP on another
  port could survive the ban and stay at the front of the reconnection order for the lifetime of
  the process ([#11173](https://github.com/ZcashFoundation/zebra/pull/11173)).

### Security

- Inbound connections are canonicalized at the accept boundary, so an IPv4 peer that connects to a
  dual-stack listener as an IPv4-mapped IPv6 address (`::ffff:A.B.C.D`) is keyed on its canonical
  IPv4 address. Previously the mapped address became the peer set key, so a ban issued for that
  peer's IPv4 address did not disconnect it while it stayed connected, and the same peer counted
  twice towards the per-IP inbound connection limit
  ([#11129](https://github.com/ZcashFoundation/zebra/pull/11129)).

## [11.0.0] - 2026-07-27

### Breaking Changes

- `HandshakeError` gains the `MissingRequiredServices` variant. `HandshakeError` is not
  `#[non_exhaustive]`, so exhaustive `match` expressions over it stop compiling
  ([#11071](https://github.com/ZcashFoundation/zebra/pull/11071)).

### Added

- `HandshakeError::MissingRequiredServices`, returned when an outbound handshake is rejected
  because the remote peer's `version` message doesn't advertise `NODE_NETWORK`
  ([#11071](https://github.com/ZcashFoundation/zebra/pull/11071)).

### Changed

- While the node is syncing, outbound connections to peers that don't advertise `NODE_NETWORK`
  are rejected during the handshake, so a fresh sync's outbound slots aren't occupied by
  non-serving peers. At or near the network tip, such peers (like pruned nodes, which can serve
  recent blocks) are accepted again. Inbound and isolated connections are unaffected
  ([#11071](https://github.com/ZcashFoundation/zebra/pull/11071)).
- The peer crawler now queues a connection attempt on each crawl interval for every spare
  outbound connection slot that has a ready address book candidate, so dropped outbound
  connections are proactively replaced until the outbound connection limit is reached
  ([#11102](https://github.com/ZcashFoundation/zebra/issues/11102)).
- Zebra now sends up to half of its address book in response to a `getaddr` request, up from
  a quarter, so peers can find more of the network from each response. The maximum address
  book size is now pinned at 5000 instead of being derived from the response fraction
  ([#11103](https://github.com/ZcashFoundation/zebra/issues/11103)).
- The peer stall detector no longer disconnects peers for empty `FindBlocks` or `FindHeaders`
  responses while the node is within 1,000 estimated blocks of the network tip
  ([#11122](https://github.com/ZcashFoundation/zebra/pull/11122)).

## [10.2.1] - 2026-07-24

### Changed

- The first peer disk-cache write is retried every 20 seconds until it succeeds, instead of
  waiting the full 5-minute update interval, so a cold-started node caches its peers soon after
  finding them ([#11073](https://github.com/ZcashFoundation/zebra/pull/11073)).

## [10.2.0] - 2026-07-17

### Added

- `init_with_block_gossip_peer_ips`, an `init` variant that treats inbound peers
  from the listed IP addresses as trusted zcashd-compat sidecars: they always receive
  `AdvertiseBlock` inventory broadcasts (queued while the peer is busy), share a
  reserved inbound connection pool of one slot per listed IP (falling back to public
  slots and the normal rate limits when the pool is full), bypass the recent-IP
  reconnection rate limit while a reserved slot is free, and are exempt from the
  `FindBlocks`/`FindHeaders` stall detector. Callers must only list IPs where every
  process is trusted
  ([#10952](https://github.com/ZcashFoundation/zebra/pull/10952)).

## [10.1.1] - 2026-07-17

### Changed

- `zebra-chain` dependency bumped to `11.2.0`.

## [10.1.0] - 2026-07-10

### Changed

- MSRV is now 1.88

## [10.0.0] - 2026-07-02

### Breaking Changes

- `Request::PushTransaction` now carries the sending peer's address as a second field
  (`Option<PeerSocketAddr>`), so directly pushed transactions are attributed to the
  sending peer and subject to the same per-peer mempool admission cap as advertised
  transaction IDs
  ([GHSA-m9xx-8rcj-vmgp](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-m9xx-8rcj-vmgp)).

### Added

- Added the Regtest-only network config option `should_allow_unshielded_coinbase_spends`,
  controlling whether coinbase outputs may be spent into transparent outputs. Setting it
  on a configured Testnet is rejected with an error
  ([#10698](https://github.com/ZcashFoundation/zebra/pull/10698)).

## [9.0.0] - 2026-06-10

### Breaking Changes

- `INITIAL_MIN_NETWORK_PROTOCOL_VERSION` bumped from `Nu6` (170120) to `Nu6_2` (170150)
  on Mainnet, Testnet, and Regtest. Peers running protocol version 170120 are no longer
  accepted ([#10692](https://github.com/ZcashFoundation/zebra/pull/10692)).
- Removed `Copy` derive from `types::MetaAddr` (now only `Clone`) to support `String` fields.
- Changed `types::MetaAddr::new_connected()` to take additional `user_agent` and
  `negotiated_version` parameters.

### Added

- `MetaAddr::user_agent()` accessor returning `Option<&str>`.
- `MetaAddr::negotiated_version()` accessor returning `Option<Version>`.
- `MetaAddr::last_connection_state()` accessor returning `PeerAddrState`.
- `MetaAddr::services()` accessor returning `Option<PeerServices>`.
- `Display` impl for `PeerAddrState`.
- Made `types::Version` type public with `Display` impl.

### Fixed

- Fixed genesis-to-tip sync stall where the peer crawler could stop receiving
  ready peers after an extended crawl period
  ([#5709](https://github.com/ZcashFoundation/zebra/issues/5709)).

## [8.0.0] - 2026-06-02

### Changed

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
