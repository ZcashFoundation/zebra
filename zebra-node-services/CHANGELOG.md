# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.1.2] - 2026-01-21

No API changes; internal dependencies updated.

## [2.1.1] - 2025-11-28

No API changes; internal dependencies updated.

## [2.1.0] - 2025-11-17

### Added

- Added helper traits for types implementing tower services ([#10010](https://github.com/ZcashFoundation/zebra/pull/10010))

## [2.0.0] - 2025-10-15

Added support for `getmempoolinfo` RPC method ([#9870](https://github.com/ZcashFoundation/zebra/pull/9870)).

### Breaking Changes

- Added `Request::QueueStats` and `Response::QueueStats` variants.
- Removed `constants` module.

## [1.0.1] - 2025-08-07

### Changed

- Bumped `zebra-chain` to `2.0.0`.

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
