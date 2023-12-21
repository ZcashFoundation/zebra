//! State contextual verification and storage code for Zebra.
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
#![doc(html_root_url = "https://docs.rs/zebra_state")]

#[macro_use]
extern crate tracing;

// TODO: only export the Config struct and a few other important methods
pub mod config;
// Most constants are exported by default
pub mod constants;

// Allow use in external tests
#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

mod error;
mod request;
mod response;
mod service;

#[cfg(test)]
mod tests;

pub use config::{
    check_and_delete_old_databases, check_and_delete_old_state_databases,
    database_format_version_on_disk, state_database_format_version_on_disk, Config,
};
pub use constants::{state_database_format_version_in_code, MAX_BLOCK_REORG_HEIGHT};
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
    OutputIndex, OutputLocation, TransactionIndex, TransactionLocation,
};

// Allow use in the scanner
#[cfg(feature = "shielded-scan")]
pub use service::finalized_state::{
    SaplingScannedDatabaseEntry, SaplingScannedDatabaseIndex, SaplingScannedResult,
    SaplingScanningKey,
};

// Allow use in the scanner and external tests
#[cfg(any(test, feature = "proptest-impl", feature = "shielded-scan"))]
pub use service::{
    finalized_state::{
        DiskWriteBatch, FromDisk, IntoDisk, ReadDisk, TypedColumnFamily, WriteDisk,
        WriteTypedBatch, ZebraDb,
    },
    ReadStateService,
};

#[cfg(feature = "getblocktemplate-rpcs")]
pub use response::GetBlockTemplateChainInfo;

// Allow use in external tests
#[cfg(any(test, feature = "proptest-impl"))]
pub use service::{
    arbitrary::{populated_state, CHAIN_TIP_UPDATE_WAIT_LIMIT},
    chain_tip::{ChainTipBlock, ChainTipSender},
    finalized_state::{RawBytes, KV, MAX_ON_DISK_HEIGHT},
    init_test, init_test_services,
};

#[cfg(any(test, feature = "proptest-impl"))]
pub use constants::latest_version_for_adding_subtrees;

#[cfg(any(test, feature = "proptest-impl"))]
pub use config::hidden::{
    write_database_format_version_to_disk, write_state_database_format_version_to_disk,
};

// Allow use only inside the crate in production
#[cfg(not(any(test, feature = "proptest-impl")))]
#[allow(unused_imports)]
pub(crate) use config::hidden::{
    write_database_format_version_to_disk, write_state_database_format_version_to_disk,
};

pub(crate) use request::ContextuallyVerifiedBlock;
