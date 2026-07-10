# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [10.1.0] - 2026-07-10

### Added

- `CommitCheckpointVerifiedError` re-exported from the crate root
- `CommitCheckpointVerifiedError::inner()` and `CommitSemanticallyVerifiedError::inner()`
  accessors for the underlying `CommitBlockError`
  ([#10916](https://github.com/ZcashFoundation/zebra/pull/10916))

### Changed

- MSRV is now 1.88
- Migrated to `rocksdb 0.24`

## [10.0.0] - 2026-07-02

### Breaking Changes

- The finalized-state open functions now return `Result<_, StateInitError>` instead
  of panicking when a read-only state cannot be opened: `FinalizedState::new`,
  `init_read_only`, `spawn_init_read_only`, and the lower-level `ZebraDb::new`.
  A read-only open against a missing or
  unreadable cache directory, with no existing database on disk, or with an ephemeral
  database also configured (a read-only secondary must not delete the primary's
  files), now returns the new public `StateInitError` rather than panicking. The
  read-write open path is unchanged.
  ([#10741](https://github.com/ZcashFoundation/zebra/pull/10741))
- `ReadRequest::NonFinalizedBlocksListener` is now a struct variant carrying the
  caller's `known_chain_tips`, so the non-finalized blocks listener streams only the
  blocks above the chain tips the caller already has. `NonFinalizedBlocksListener::spawn`
  takes the same `known_chain_tips` set and no longer takes a `Network`.
  `MAX_NON_FINALIZED_CHAIN_FORKS` is now re-exported from the crate root.
- Added `ReadRequest::FindForkPoint { known_blocks }` request and the corresponding
  `ReadResponse::ForkPoint(Option<(block::Height, block::Hash)>)` response. The
  server returns the most recent block in the caller-supplied locator that is
  on the best chain (the fork point) to assist in reorg handling for clients
  that track only a single chain tip at a time.
  ([#10764](https://github.com/ZcashFoundation/zebra/pull/10764)).

### Added

- `request::Spend::Ironwood`
- `impl From<ironwood::Nullifier> for Spend`
- `ReadRequest::IronwoodTree` and `ReadRequest::IronwoodSubtrees { start_index, limit }`, with
  the corresponding `ReadResponse::{IronwoodTree, IronwoodSubtrees}` responses.
- `ValidateContextError::{DuplicateIronwoodNullifier, UnknownIronwoodAnchor}`
- `DiskWriteBatch::{create_ironwood_tree, insert_ironwood_subtree}`
- `ZebraDb`:
  - `contains_ironwood_anchor`
  - `contains_ironwood_nullifier`
  - `ironwood_revealing_tx_loc`
  - `ironwood_subtree_list_by_index_range`
  - `ironwood_tree_by_hash_or_height`
  - `ironwood_tree_by_height`
  - `ironwood_tree_by_height_range`
  - `ironwood_tree_for_tip`
- `impl IntoDisk for ironwood::Nullifier`
- `impl DuplicateNullifierError for ironwood::Nullifier`

### Changed

- Bumped the on-disk database format version (27 → 28) for the Ironwood note
  commitment tree, anchors, subtrees, and nullifier set.
- `IntoDisk for ValueBalance<NonNegative>` now serializes to `[u8; 48]`
  (was `[u8; 40]`), for the added Ironwood pool balance.

### Fixed

- Read-only secondary databases no longer attempt to flush on shutdown, which RocksDB
  rejects and which was logged as an unexpected error
  ([#10784](https://github.com/ZcashFoundation/zebra/pull/10784))
- Finalizing a block no longer re-inserts an emptied side chain into the chain set
  ([#10818](https://github.com/ZcashFoundation/zebra/pull/10818))

## [9.0.1] - 2026-06-18

### Changed

- Increased the local rollback window (`MAX_BLOCK_REORG_HEIGHT`) from 99 to 1000
  blocks as a defence-in-depth measure against sustained consensus splits
  ([#10650](https://github.com/ZcashFoundation/zebra/pull/10650))

## [9.0.0] - 2026-06-10

### Breaking Changes

- Removed `deferred_pool_balance_change` field from `ContextuallyVerifiedBlock`,
  `SemanticallyVerifiedBlock`, and `CheckpointVerifiedBlock`. The deferred pool
  balance change is now calculated on demand and passed as a parameter to
  `FinalizedBlock::from_checkpoint_verified()` and
  `FinalizedBlock::from_contextually_verified()`.
- `CheckpointVerifiedBlock::new()` no longer takes a `deferred_pool_balance_change`
  parameter.

### Fixed

- `QueuedBlocks::dequeue_children()`: fixed `by_height` index handling to remove
  individual hashes instead of the entire height entry, preventing loss of queued
  blocks at the same height
  ([#10604](https://github.com/ZcashFoundation/zebra/pull/10604)).

## [8.0.0] - 2026-06-02

### Changed

- Release for NU6.2 support (updates `zebra-chain` to 9.0.0). Internal refactor of the
  chain-tip mempool-reset height computation; no public API or behavior change.

## [7.0.0] - 2026-05-28

This release fixes four state security issues:

- Drop rejected block hashes from `SentHashes` so honest re-deliveries of a
  block are no longer short-circuited as duplicates
  ([GHSA-4m69-67m6-prqp](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-4m69-67m6-prqp).
- Apply transparent address-balance updates per-transaction in
  debit-before-credit order so same-address self-spend chains do not push
  intermediate balances above `MAX_MONEY` and panic
  ([GHSA-w834-cf6p-9m9w](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-w834-cf6p-9m9w)).
- Pop the matching Sapling/Orchard subtree when popping a non-finalized tip
  that completed one
  ([GHSA-2gf8-q9rr-jq3h](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-2gf8-q9rr-jq3h)).
- Reject repeated shielded transactions cleanly before the defence-in-depth
  `tx_loc_by_hash` assertion
  ([GHSA-hhm7-qrv5-h4r6](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-hhm7-qrv5-h4r6)).

The impact of these issues for crate users will depend on the particular
usage; if you use it as a building block for a consensus node, you should
update.

### Added

- `CommitBlockError::misbehavior_score(&self) -> u32` (currently always `0`;
  mirrors the misbehavior-score API in `zebra-consensus` /
  `zebra-network`).
- `SentHashes::remove(&mut self, hash: &block::Hash)`, used to drop rejected
  block hashes and their tracked outpoints/buffers.

### Changed

- `service::write::BlockWriteSender::spawn` return tuple gained a fourth
  element: an `UnboundedReceiver<block::Hash>` that delivers hashes of
  write-task-rejected non-finalized blocks so the `StateService` can clear
  them from `SentHashes`.
- `zebra-chain` dependency bumped to `8.0.0`.
- `zebra-node-services` dependency bumped to `6.0.0`.

## [6.0.0] - 2026-05-01

### Changed

- `zebra-chain` bumped to `7.0.0`. No direct public-API changes in this crate,
  but consumers of re-exported `zebra-chain` items (e.g. `constants::MIN_TRANSPARENT_COINBASE_MATURITY`)
  inherit that crate's breaking changes.

## [5.0.0] - 2026-03-12

### Breaking Changes

- `zebra-chain` bumped to `6.0.0`
- `Config` added the public field `debug_skip_non_finalized_state_backup_task`
- `NonFinalizedState::with_backup` now takes 5 parameters instead of 4
- Added new variants to public enums:
  - `ReadRequest::IsTransparentOutputSpent`
  - `ReadResponse::IsTransparentOutputSpent`
  - `Request::AnyChainBlock`
  - `ReadRequest::AnyChainBlock`

### Added

- Added `ReadRequest::IsTransparentOutputSpent` and `ReadResponse::IsTransparentOutputSpent` to the read state service
- Added `{ReadRequest, Request}::AnyChainBlock` to the read state service

## [4.0.0] - 2026-02-05

### Breaking Changes

- `zebra-chain` bumped to 5.0.0.
- `CommitSemanticallyVerifiedError` changed from enum to struct.
- `DiskWriteBatch::prepare_*` methods now return `()` or specific error types instead of `Result<(), BoxError>`.
- `FinalizedState::commit_finalized*` methods now return `CommitCheckpointVerifiedError`.

### Added

- `CommitBlockError` enum with `Duplicate`, `ValidateContextError`, `WriteTaskExited` variants.
- `KnownBlock::Finalized` and `KnownBlock::WriteChannel` variants.
- `impl From<ValidateContextError> for CommitSemanticallyVerifiedError`.
- Added the concrete error type `CommitCheckpointVerifiedError` for handling failed state requests during checkpoint verification ([#9979](https://github.com/ZcashFoundation/zebra/pull/9979)) _(documented after release)_
- Added `MappedRequest` for `CommitCheckpointVerifiedBlockRequest` ([#9979](https://github.com/ZcashFoundation/zebra/pull/9979)) _(documented after release)_

## [3.1.2] - 2026-01-21 - Yanked

This should have been a major release, see 4.0.0.

Dependencies updated.

## [3.1.1] - 2025-11-28

No API changes; internal dependencies updated.

## [3.1.0] - 2025-11-17

### Added

- Added `State` and `ReadState` helper traits for convenience when constraining generic type parameters ([#10010](https://github.com/ZcashFoundation/zebra/pull/10010))
- Made `response::NonFinalizedBlocksListener` publicly accessible ([#10083](https://github.com/ZcashFoundation/zebra/pull/10083))

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
