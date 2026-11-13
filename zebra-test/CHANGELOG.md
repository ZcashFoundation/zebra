# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org).

## [5.0.0] - 2026-10-22

### Breaking Changes

- Enforced `command::TestChild::with_timeout` for quiet and partial-line reads and natural process exit. It now returns a `Result`, rejecting unrepresentable deadlines with `InvalidInput` rather than panicking. Made `TestChild::{deadline, timeout}` private; use `with_timeout` to change the budget and `deadline()` to inspect it. Reset timeouts between startup and longer observation phases; existing output readers are updated immediately. `wait_with_output` drains both remaining streams, with an independent one-second cleanup grace after intentional termination or natural exit. Cleanup no longer requires renewing the observation timeout. Changed `TestChild::{stdout, stderr}` to `Option<command::ChildOutput>` and removed `command::IteratorDebug`; use the `TestChild` output methods instead of custom iterators. Bounded reaping and draining in `TestChild::{kill_and_return_output, kill_and_consume_output}` to one second; returned output retains line endings. ([#11598](https://github.com/ZcashFoundation/zebra/pull/11598))
- Run spawned test commands in an owned Unix process group or Windows job, terminating remaining members before reaping the leader. Removed `command::TestChild::child`; use the process ID, piped-input, and Windows borrowed-handle accessors instead. Unix callers must not externally reap the leader or enable SIGCHLD auto-reaping; descendants leaving the group are outside cleanup ownership. Windows callers must leave creation flags at their default zero. ([#11598](https://github.com/ZcashFoundation/zebra/pull/11598))

### Added

- Added `command::TestChild::{id, stdin_mut}` and Windows `AsHandle` access without exposing process reaping. ([#11598](https://github.com/ZcashFoundation/zebra/pull/11598))

### Changed

- Raised the MSRV to 1.89.

### Fixed

- `TestChild::wait_with_output` drains output already taken by line readers while waiting for the child to exit, preventing deadlocks when its stdout or stderr pipe fills.

## [4.0.0] - 2026-07-02

### Removed

- The `tx_v6` cargo feature; v6 transaction test support no longer needs a feature gate
  ([#10762](https://github.com/ZcashFoundation/zebra/pull/10762))

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
