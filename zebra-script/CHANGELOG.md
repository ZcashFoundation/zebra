# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [4.0.0] - 2026-02-04

- `zebra-chain` was bumped to `4.0.0`, requiring a major release

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

- Removed  `legacy_sigop_count`; use the `Sigops` trait instead.

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
