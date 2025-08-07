# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
