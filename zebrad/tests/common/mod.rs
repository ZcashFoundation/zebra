//! Shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file and its submodules will not be run.
//! This file is only for test library code.
//!
//! This module uses the legacy directory structure,
//! to avoid compiling an empty "common" test binary:
//! <https://doc.rust-lang.org/book/ch11-03-test-organization.html#submodules-in-integration-tests>

pub mod cached_state;
pub mod check;
pub mod config;
pub mod failure_messages;
pub mod launch;
pub mod lightwalletd;
pub mod sync;
pub mod test_type;

#[cfg(feature = "zebra-checkpoints")]
pub mod checkpoints;

#[cfg(feature = "getblocktemplate-rpcs")]
pub mod get_block_template_rpcs;
