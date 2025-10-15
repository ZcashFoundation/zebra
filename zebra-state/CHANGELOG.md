# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [3.0.0] - 2025-10-15

This release adds new request and response variants for transaction lookups to support new RPC methods, introduces a configuration option for backing up the non-finalized state, and refactors error handling for improved type safety and clarity.
Additionally, it fixes a regression in Zebra’s sync performance that was introduced when avoiding the use of the RocksDB merge operator.

### Breaking Changes

- Added new configuration field `Config::should_backup_non_finalized_state`
- Added new request and response enum variants:
  - `Request::AnyChainTransaction`
  - `ReadRequest::{AnyChainTransaction, AnyChainTransactionIdsForBlock}`
  - `Response::AnyChainTransaction`
  - `ReadResponse::{AnyChainTransaction, AnyChainTransactionIdsForBlock}`
- Changed `CommitSemanticallyVerifiedError` from a struct to an enum (#9923).
- Removed the `public spawn_init` function.
- Updated error messages in response to failed `CommitSemanticallyVerifiedBlock` state requests ([#9923](https://github.com/ZcashFoundation/zebra/pull/9923))

## Added

- Added `MappedRequest` trait and `CommitSemanticallyVerifiedBlockRequest` for convenient state response and error type conversions ([#9923](https://github.com/ZcashFoundation/zebra/pull/9923))

## Fixed

- Restore initial sync performance by avoiding RocksDB merge operations when the on-disk database format is up-to-date ([#9973](https://github.com/ZcashFoundation/zebra/pull/9973))
- Replaced boxed-string errors in response to failed `CommitSemanticallyVerifiedBlock` and `ReconsiderBlock` state requests with concrete error type ([#9848](https://github.com/ZcashFoundation/zebra/pull/9848), [#9923](https://github.com/ZcashFoundation/zebra/pull/9923), [#9919](https://github.com/ZcashFoundation/zebra/pull/9919))


## [2.0.0] - 2025-08-07

### Breaking Changes

- Renamed `SemanticallyVerifiedBlock::with_deferred_balance()` to
  `with_deferred_pool_balance_change()`
- Renamed `SemanticallyVerifiedBlock::deferred_balance` to
  `SemanticallyVerifiedBlock::deferred_pool_balance_change`


## [1.0.1] - 2025-07-22

### Fixed

- Fix 2.4.0 DB upgrade; add warning if impacted ([#9709](https://github.com/ZcashFoundation/zebra/pull/9709)).
  See the Zebra changelog for more details.


## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
