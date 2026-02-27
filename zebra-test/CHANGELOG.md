# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
