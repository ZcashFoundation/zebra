//! Zebra integration and acceptance tests.
//!
//! ## Test categories
//!
//! Tests are organized by module path into three tiers:
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
//! ## Running tests
//!
//! ```console
//! # All fast tests (unit + integration) — default profile excludes stateful:
//! cargo nextest run
//!
//! # Specific category:
//! cargo nextest run -E 'test(/^unit::/)'
//! cargo nextest run -E 'test(/^integration::/)'
//!
//! # Specific test:
//! cargo nextest run -E 'test(sync_one_checkpoint_mainnet)'
//!
//! # CI profiles:
//! cargo nextest run --profile ci                                                # PR tests
//! cargo nextest run --profile ci-stateful -E 'test(sync_full_mainnet)'          # GCP
//! ```

#![allow(clippy::unwrap_in_result)]

#[macro_use]
mod common;
mod integration;
mod stateful;
mod unit;
