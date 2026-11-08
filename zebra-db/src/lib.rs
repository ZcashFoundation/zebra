//! Finalized-state storage and state snapshots for Zebra.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_db")]
// Remove if possible if MSRV is increased
#![allow(unknown_lints)]
#![allow(clippy::manual_is_multiple_of)]

#[macro_use]
extern crate tracing;

pub mod config;
pub mod constants;
#[allow(missing_docs)]
pub mod error;
mod request;
mod response;
pub mod service;
pub mod state_snapshot;

pub use config::{database_format_version_on_disk, Config, RedactedString};
pub use constants::*;
pub use error::*;
#[cfg(feature = "indexer")]
pub use request::Spend;
pub use request::{
    CheckpointVerifiedBlock, CommitAids, FinalizedBlock, HashOrHeight, SemanticallyVerifiedBlock,
    Treestate,
};
pub use response::{HistoryCapabilities, KnownBlock};
pub use service::finalized_state::{
    restorable_db_versions, DiskDb, DiskWriteBatch, FromDisk, IntoDisk, OutputLocation, RawBytes,
    ReadDisk, TransactionIndex, TransactionLocation, TypedColumnFamily, WriteDisk, WriteTypedBatch,
    ZebraDb, MAX_ON_DISK_HEIGHT,
};
pub use state_snapshot::SnapshotBase;

pub use config::hidden::write_database_format_version_to_disk;
