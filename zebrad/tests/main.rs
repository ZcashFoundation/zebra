//! Zebrad tests.
//!
//! ## Test categories
//!
//! Tests are organized by module path into four tiers:
//!
//! - **`unit::`** — Fast tests for CLI, config, basic functionality (<1 min).
//!   No network or state required.
//!
//! - **`integration::`** — Tests that launch zebrad, check local endpoints,
//!   verify database behavior, exercise regtest mode, or run bounded sync
//!   checks. No cached blockchain state or runtime lightwalletd dependency
//!   required.
//!
//! - **`stateful::`** — Tests that require a cached blockchain state directory
//!   or runtime lightwalletd dependency. Run on GCP VMs with persistent disks.
//!   (30 min – days)
//!
//! - **`e2e::`** — Full-system tests that depend on public-network peer
//!   availability, such as syncs, peer RPCs, trusted-chain checks, checkpoint
//!   generation, and lightwalletd full syncs. Run on scheduled or manually
//!   selected GCP jobs. (hours – days)
//!
//! ### Opt-in integration tests
//!
//! Some `integration::` tests run several nodes through thousands of Regtest
//! blocks, so the `default` and `ci` nextest profiles leave them out with their
//! `default-filter` in `.config/nextest.toml`, and no CI job runs them:
//!
//! - `integration::v2_checkpoint_sync::`
//! - `integration::headers_first_sync::`
//!
//! They are also `#[ignore]`d. Run them with `--ignore-default-filter --run-ignored=all`, in
//! release, one at a time:
//!
//! ```console
//! cargo nextest run --release -p zebrad --ignore-default-filter --run-ignored=all \
//!     --test-threads 1 \
//!     -E 'test(/^integration::(v2_checkpoint_sync|headers_first_sync)::/)'
//! ```
//!
//! ## Running tests
//!
//! ```console
//! # All fast tests (unit + integration) — default profile excludes stateful:
//! cargo nextest run
//!
//! # Specific category:
//! cargo nextest run -E 'test(/^unit::/)'
//! cargo nextest run -E 'test(/^integration::/)'
//! cargo nextest run --profile ci-stateful --run-ignored=all -E 'test(/^stateful::/)'
//! cargo nextest run --profile ci-e2e --run-ignored=all -E 'test(/^e2e::/)'
//!
//! # Specific test:
//! cargo nextest run -E 'test(=integration::sync::sync_one_checkpoint_mainnet)'
//!
//! # CI profiles:
//! cargo nextest run --profile ci                                           # PR tests
//! cargo nextest run --profile ci-stateful --run-ignored=all -E 'test(=stateful::sync::sync_update_mainnet)'
//! cargo nextest run --profile ci-e2e --run-ignored=all -E 'test(=e2e::sync::sync_full_mainnet)'
//! ```

#![allow(clippy::unwrap_in_result)]

#[macro_use]
mod common;
mod e2e;
mod integration;
mod stateful;
mod unit;
