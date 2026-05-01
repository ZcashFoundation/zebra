# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

This release fixes an important security issue:

- [CVE-2026-XXXXX: Consensus Divergence in Transparent Sighash Hash-Type
  Handling due to Stale
  Buffer](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-gq4h-3grw-2rhv)

The impact of the issue for crate users will depend on the particular usage;
if you use it as a building block for a consensus node, you should update.

### Breaking Changes

- `Sigops::scripts` now returns `impl Iterator<Item = Vec<u8>>` instead of `impl
  Iterator<Item = &[u8]>`. This signature change ripples through all `Sigops`
  implementations:
  - `impl Sigops for zebra_chain::transaction::Transaction`
  - `impl Sigops for zebra_chain::transaction::UnminedTx`
  - `impl Sigops for CachedFfiTransaction`
  - `impl Sigops for zcash_primitives::transaction::Transaction`

### Added

- `CachedFfiTransaction::p2sh_sigops(&self) -> u32`.
- `p2sh_sigop_count(tx, spent_outputs) -> u32`.

## [6.0.0] - PLANNED

### Breaking changes

- Migrated to `zcash_primitives 0.27` (and the rest of the librustzcash 2026-04 release wave), which replaces the yanked `core2` dependency with `corez`.

## [5.0.1] - 2026-04-17

This release fixes an important security issue:

- [CVE-2026-XXXXX: Consensus Divergence in Transparent Sighash Hash-Type Handling](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-8m29-fpq5-89jj)

The impact of the issue for crate users will depend on the particular usage;
if you use it as a building block for a consensus node, you should update.

## [5.0.0] - 2026-03-12

- `zebra-chain` was bumped to `6.0.0`

## [4.0.0] - 2026-02-04

- `zebra-chain` was bumped to 5.0.0, requiring a major release

## [3.0.2] - 2026-01-21 - Yanked

This should have been a major release, see 4.0.0.

Dependencies updated.

## [3.0.1] - 2025-11-28

No API changes; internal dependencies updated.

## [3.0.0] - 2025-10-15

### Breaking changes

- Updated to the last version of `zcash_script` ([#9927](https://github.com/ZcashFoundation/zebra/pull/9927))

## [2.0.0] - 2025-08-07

### Breaking Changes

- Removed `legacy_sigop_count`; use the `Sigops` trait instead.

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
