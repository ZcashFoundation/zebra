//! Zebrad tests.
//!
//! ## Test categories
//!
//! Tests are organized by module path into four tiers:
//!
//! - **`unit::`** — Fast tests for CLI, config, basic functionality (<1 min).
//!   No network or state required.
//!
//! - **`integration::`** — Tests that launch zebrad, check endpoints, verify
//!   database behavior, or exercise regtest mode. No cached blockchain state
//!   required. (5-15 min)
//!
//! - **`stateful::`** — Tests that require a cached blockchain state directory
//!   and/or external services like lightwalletd. Run on GCP VMs with persistent
//!   disks. (30 min – days)
//!
//! - **`e2e::`** — Full-system tests such as public-network syncs,
//!   lightwalletd full syncs, and checkpoint generation. Run on scheduled or
//!   manually selected GCP jobs. (hours – days)
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
//! cargo nextest run -E 'test(/^stateful::/)'
//! cargo nextest run -E 'test(/^e2e::/)'
//!
//! # Specific test:
//! cargo nextest run -E 'test(=integration::sync::sync_one_checkpoint_mainnet)'
//!
//! # CI profiles:
//! cargo nextest run --profile ci                                           # PR tests
//! cargo nextest run --profile ci-stateful -E 'test(=stateful::sync::sync_update_mainnet)'
//! cargo nextest run --profile ci-e2e -E 'test(=e2e::sync::sync_full_mainnet)'
//! ```

#![allow(clippy::unwrap_in_result)]

#[macro_use]
mod common;
mod e2e;
mod integration;
mod stateful;
mod unit;
