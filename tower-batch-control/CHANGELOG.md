# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] - 2026-07-10

### Added

- Added a `tonic` feature that implements `RequestWeight` for `tonic::Request<T>`,
  so gRPC requests can be used as the batch request type without consumer-side
  newtype wrappers ([#10667](https://github.com/ZcashFoundation/zebra/issues/10667))

### Changed

- MSRV is now 1.88

## [0.2.42] - 2025-10-15

### Added

- Added `RequestWeight` trait and changed to a `max_items_weight_in_batch` field in the
  `BatchLayer` struct ([#9308](https://github.com/ZcashFoundation/zebra/pull/9308))

## [0.2.41] - 2025-07-11

First "stable" release. However, be advised that the API may still greatly
change so major version bumps can be common.
