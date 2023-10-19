//! State contextual verification and storage code for Zebra. 🦓
//!
//! # Correctness
//!
//! Await UTXO and block commit requests should be wrapped in a timeout, because:
//! - await UTXO requests wait for a block containing that UTXO, and
//! - contextual verification and state updates wait for all previous blocks.
//!
//! Otherwise, verification of out-of-order and invalid blocks can hang indefinitely.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
//
// Rust 1.72 has a false positive when nested generics are used inside Arc.
// This makes the `arc_with_non_send_sync` lint trigger on a lot of proptest code.
//
// TODO: remove this allow when Rust 1.73 is stable, because this lint bug is fixed in that release:
// <https://github.com/rust-lang/rust-clippy/issues/11076>
#![cfg_attr(
    any(test, feature = "proptest-impl"),
    allow(clippy::arc_with_non_send_sync)
)]

#[macro_use]
extern crate tracing;

pub mod constants;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

mod config;
mod error;
mod request;
mod response;
mod service;

#[cfg(test)]
mod tests;

pub use config::{
    check_and_delete_old_databases, database_format_version_in_code,
    database_format_version_on_disk, Config,
};
pub use constants::MAX_BLOCK_REORG_HEIGHT;
pub use error::{
    BoxError, CloneError, CommitSemanticallyVerifiedError, DuplicateNullifierError,
    ValidateContextError,
};
pub use request::{
    CheckpointVerifiedBlock, HashOrHeight, ReadRequest, Request, SemanticallyVerifiedBlock,
};
pub use response::{KnownBlock, MinedTx, ReadResponse, Response};
pub use service::{
    chain_tip::{ChainTipChange, LatestChainTip, TipAction},
    check, init, spawn_init,
    watch_receiver::WatchReceiver,
    OutputIndex, OutputLocation, TransactionLocation,
};

#[cfg(feature = "getblocktemplate-rpcs")]
pub use response::GetBlockTemplateChainInfo;

#[cfg(any(test, feature = "proptest-impl"))]
pub use service::{
    arbitrary::{populated_state, CHAIN_TIP_UPDATE_WAIT_LIMIT},
    chain_tip::{ChainTipBlock, ChainTipSender},
    finalized_state::{DiskWriteBatch, MAX_ON_DISK_HEIGHT},
    init_test, init_test_services, ReadStateService,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub use config::write_database_format_version_to_disk;

#[cfg(any(test, feature = "proptest-impl"))]
pub use constants::latest_version_for_adding_subtrees;

pub(crate) use request::ContextuallyVerifiedBlock;
