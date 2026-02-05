# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [Unreleased]

### Fixed

- Fixed the "Outbound Connections" progress bar spiking to the connection limit during peer crawl cycles (#7981).


## [3.0.0] - 2026-01-21

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
