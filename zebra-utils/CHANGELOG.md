# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [8.1.0](https://github.com/ZcashFoundation/zebra/compare/zebra-utils-v8.0.1...zebra-utils-v8.1.0) - 2026-06-25


### Added

- Add check-api script for public-API and dependency diffs ([#10721](https://github.com/ZcashFoundation/zebra/pull/10721))

## [8.0.1] - 2026-06-18

### Changed

- `zebra-rpc` dependency bumped to `10.0.1`
- `zebra-checkpoints` now backs off `MAX_BLOCK_REORG_HEIGHT` (1000) instead of the coinbase
  maturity (100) when choosing the highest checkpoint.

## [8.0.0] - 2026-06-10

### Changed

- `zebra-chain` dependency bumped to `10.0.0`, `zebra-rpc` to `10.0.0`, and
  `zebra-node-services` to `8.0.0`. No other changes to this crate.

## [7.0.0] - 2026-06-02

### Changed

- Update to `zebra-chain` 9.0.0, `zebra-rpc` 9.0.0, and `zebra-node-services` 7.0.0
  (NU6.2 support). No other changes to this crate.

## [6.0.0] - 2026-05-01

### Changed

- `zebra-chain` bumped to `7.0.0`.
- `zebra-rpc` bumped to `7.0.0`.
- `zebra-node-services` bumped to `5.0.0`.

## [5.0.0] - 2026-03-12

### Breaking Changes

- `zebra-chain` was bumped to 6.0.0
- `zebra-rpc` was bumped to 6.0.0
- `zebra-node-services` bumped to 4.0.0

### Removed

- Removed Cargo features:
  - `serde`
  - `serde_yml`
  - `openapi-generator`
  - `quote`
  - `syn`
- The `openapi-generator` tool was removed in favor of OpenRPC.

## [4.0.0] - 2026-02-05

- `zebra-chain` was bumped to 5.0.0
- `zebra-rpc` was bumped to 5.0.0
- `zebra-node-services` bumped to 3.0.0

## [3.0.2] - 2026-01-21

This should have been a major release, see 4.0.0.

Dependencies updated.

## [3.0.1] - 2025-11-28

No API changes; internal dependencies updated.

## [3.0.0] - 2025-10-15

- Removed unused `jsonrpc` dependency.

## [2.0.0] - 2025-08-07

## Breaking Changes

- Removed unused `zcash_client_backend` dependency.

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
