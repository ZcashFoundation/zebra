# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-07-17

### Added

- `Fallback::new_with_policy`, which selects fallback behavior using a `FallbackPolicy`.
- `FallbackPolicy` trait with an `OnError` implementation matching the previous
  fall-back-on-error behavior.

### Changed

- `Fallback` and `future::ResponseFuture` now carry a fallback-policy type parameter. The
  `Fallback::new` constructor keeps the previous fall-back-on-error semantics via the
  default policy.

## [0.2.42] - 2026-07-10

### Changed

- MSRV is now 1.88

## [0.2.41] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
