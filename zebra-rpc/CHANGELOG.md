# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [3.0.0] - XXXX-XX-XX

### Breaking Changes

TODO: note to whoever is doing the next release: we can remove the breaking
change below by adding new `new()` methods (e.g. `new_with_sprout()`). But if
there are other breaking changes we might as well save us the trouble. (Please
delete this before the release.)

- Changed the `GetTreestateResponse::new()` to take an optional Sprout tree state;
  changed `Commitments::new()` to take the `final_root` parameter.
  
### Changed

- Removed dependency on `protoc`

## [2.0.0] - 2025-08-07

### Breaking Changes

- Changed the `deferred` value pool identifier to `lockbox` in `getblock` and
  `getblockchaininfo`.

### Changed

- Slice `[GetBlockchainInfoBalance; 5]` type is aliased as `BlockchainValuePoolBalances`

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
