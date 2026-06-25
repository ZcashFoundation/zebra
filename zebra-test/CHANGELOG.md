# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [4.0.0](https://github.com/ZcashFoundation/zebra/compare/zebra-test-v3.0.0...zebra-test-v4.0.0) - 2026-06-25


### Added

- Support ZIP-213 ([#10048](https://github.com/ZcashFoundation/zebra/pull/10048))

### Fixed

- Update README to reference zebrad installation tag v4.3.0 ([#10432](https://github.com/ZcashFoundation/zebra/pull/10432))

### Release

- Zebra 4.3.1 ([#10495](https://github.com/ZcashFoundation/zebra/pull/10495))
- V4.4.1, reject v5 SIGHASH_SINGLE without a corresponding output  ([#10542](https://github.com/ZcashFoundation/zebra/pull/10542))
- V4.5.0 ([#10647](https://github.com/ZcashFoundation/zebra/pull/10647))
- V4.5.1 ([#10657](https://github.com/ZcashFoundation/zebra/pull/10657))
- V4.5.3
- Zebra v5.1.0 ([#10707](https://github.com/ZcashFoundation/zebra/pull/10707))
- Zebra v5.1.1 ([#10711](https://github.com/ZcashFoundation/zebra/pull/10711))
- Zebra v5.2.0 ([#10731](https://github.com/ZcashFoundation/zebra/pull/10731))

## [3.0.0] - 2026-03-12

### Breaking Changes

- `mock_service::MockService::expect_no_requests` now returns `()` instead of its previous return type.

## [2.0.0] - 2025-10-15

Test utilities were streamlined and Orchard vector fixtures were reorganized as an effort to
use `orchard::bundle::BatchValidator` in the transaction verifier ([#9308](https://github.com/ZcashFoundation/zebra/pull/9308)).

## Breaking Changes

- `MockService::expect_no_requests` now returns `()` (it previously returned a value).
- Removed now-unused test data ([#9308](https://github.com/ZcashFoundation/zebra/pull/9308))

## [1.0.1] - 2025-08-07

## Changed

- Clippy fixes

## [1.0.0] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
