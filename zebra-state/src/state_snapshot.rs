//! State snapshots: a pruned node's finalized state at a height `H`, exported
//! as content-addressed pieces that another node downloads and verifies.
//!
//! - [`mod@format`] defines the manifest and the piece records;
//! - [`store`] keeps the verified objects on disk, by hash;
//! - [`export`] writes a snapshot from a finalized database;
//! - [`import`] builds a finalized database from verified pieces.
//!
//! References like `(design, "Formats")` name the sections of the design page,
//! `book/src/dev/state-snapshot-sync.md`.

use std::{io, path::PathBuf};

use semver::Version;
use thiserror::Error;

use zebra_chain::{block, state_snapshot::ObjectHash};

use crate::{BoxError, StateInitError};

pub mod export;
pub mod format;
pub mod import;
pub mod store;

pub use zebra_db::state_snapshot::SnapshotBase;

/// An error while exporting, storing, or encoding a state snapshot.
#[derive(Debug, Error)]
pub enum SnapshotError {
    /// A manifest or piece does not follow the snapshot format.
    #[error("invalid snapshot format: {0}")]
    Format(#[from] format::FormatError),

    /// Reading or writing a snapshot file failed.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The file or directory the operation was on.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// An object is larger than any object the store holds.
    #[error(
        "snapshot object of {len} bytes is over the limit of {limit} bytes",
        limit = store::MAX_OBJECT_BYTES
    )]
    ObjectTooLarge {
        /// The object's length.
        len: usize,
    },

    /// An object was stored under a hash that is not its own.
    #[error("snapshot object has hash {actual}, not {expected}")]
    HashMismatch {
        /// The hash it was stored under.
        expected: ObjectHash,
        /// Its hash.
        actual: ObjectHash,
    },

    /// The state could not be opened.
    #[error("opening the state failed: {0}")]
    StateOpen(#[from] StateInitError),

    /// The state's history capabilities could not be read.
    #[error("reading the state's history capabilities failed: {0}")]
    HistoryRead(#[source] StateInitError),

    /// A component's export stopped because another component failed, whose
    /// error is reported instead.
    #[error("the export was cancelled because another component failed")]
    Cancelled,

    /// Reading the state failed.
    #[error("reading the state failed: {0}")]
    StateRead(#[from] rocksdb::Error),

    /// The state's format version could not be read.
    #[error("reading the state's format version failed: {source}")]
    FormatVersion {
        /// The underlying error.
        source: BoxError,
    },

    /// This build indexes spending transactions, which snapshots never carry.
    #[error(
        "this zebrad was built with the `indexer` feature: \
         export state snapshots with a build without it"
    )]
    IndexerBuild,

    /// The state has a spending transaction index, which snapshots never
    /// carry.
    #[error(
        "the state has a spending transaction index (format {disk_version}): \
         open it once with a zebrad built without the `indexer` feature, then export"
    )]
    SpendingIndex {
        /// The state's format version, whose build metadata names the index.
        disk_version: Version,
    },

    /// The state is in a newer format than this build writes, whose values
    /// this build may not canonicalize correctly.
    #[error(
        "the state's format {disk_version} is newer than this zebrad's {running_version}: \
         export it with a newer zebrad"
    )]
    NewerFormat {
        /// The state's format version.
        disk_version: Version,
        /// This build's format version.
        running_version: Version,
    },

    /// The state needs a format migration before this build can read it.
    #[error(
        "the state's format {disk_version} needs a migration to {running_version}: \
         start zebrad once to upgrade the state, then export"
    )]
    NeedsMigration {
        /// The state's format version.
        disk_version: Version,
        /// This build's format version.
        running_version: Version,
    },

    /// A snapshot is taken above genesis.
    #[error("a state snapshot must be taken above the genesis block")]
    GenesisHeight,

    /// The state has no blocks.
    #[error("the state is empty")]
    EmptyState,

    /// The state's finalized tip is not the snapshot height.
    #[error(
        "the finalized tip is at height {tip:?}, not at the snapshot height {height:?}: \
         stop the node at the snapshot height, for example with `debug_stop_at_height`"
    )]
    TipMismatch {
        /// The requested snapshot height.
        height: block::Height,
        /// The state's finalized tip height.
        tip: block::Height,
    },

    /// The state was pruned above the first block a snapshot ships.
    #[error(
        "the state has no block bodies below height {lowest_retained_height:?}, \
         but the snapshot window starts at {window_start:?}"
    )]
    PrunedBelowWindow {
        /// The first height of the snapshot window.
        window_start: block::Height,
        /// The first height the state has a block body for.
        lowest_retained_height: block::Height,
    },

    /// The state has no column family the snapshot reads.
    #[error("the state has no {0} column family")]
    MissingColumnFamily(&'static str),

    /// The state's rows are missing or contradict each other, so it cannot be
    /// exported.
    #[error("the state's {component} rows are inconsistent: {reason}")]
    Inconsistent {
        /// The component whose rows are inconsistent.
        component: format::ComponentId,
        /// What is wrong.
        reason: String,
    },

    /// The snapshot failed an import check. Unless the check is that a piece
    /// file or the manifest matches the commitment, the snapshot's contents
    /// are wrong for this node, so the caller marks its manifest rejected.
    #[error("state snapshot refused by the {check} check: {detail}")]
    Refused {
        /// The check that failed.
        check: import::ImportCheck,
        /// What the check found.
        detail: String,
    },

    /// A verified piece the import needs, or the SST file built from it, is
    /// not available.
    #[error("piece {index} of {component} ({hash}) is not available")]
    MissingPiece {
        /// The piece's component.
        component: format::ComponentId,
        /// The piece's index in its component.
        index: u32,
        /// The piece's hash.
        hash: ObjectHash,
    },

    /// This node cannot import a state snapshot with its configuration, build
    /// or existing state.
    #[error("cannot import a state snapshot: {0}")]
    Unsupported(String),

    /// A database operation failed.
    #[error("database error while {operation}: {source}")]
    Database {
        /// The operation that failed.
        operation: &'static str,
        /// The underlying error.
        #[source]
        source: rocksdb::Error,
    },

    /// The imported database could not be opened.
    #[error("cannot open the imported state: {0}")]
    Open(crate::StateInitError),

    /// The snapshot's `history_tree` value does not decode, is not canonical,
    /// or is for another network or a pre-Heartwood height.
    #[error("invalid snapshot history tree: {0}")]
    InvalidHistoryTree(String),
}

impl SnapshotError {
    /// Returns whether the error is the snapshot's fault, rather than this
    /// node's: the snapshot is malformed or failed an import check, so it is
    /// never imported again.
    pub fn rejects_snapshot(&self) -> bool {
        match self {
            SnapshotError::Refused { check, .. } => check.rejects_snapshot(),
            SnapshotError::Format(format::FormatError::Io(_)) => false,
            SnapshotError::Format(_) | SnapshotError::InvalidHistoryTree(_) => true,
            SnapshotError::Io { .. }
            | SnapshotError::MissingPiece { .. }
            | SnapshotError::Unsupported(_)
            | SnapshotError::Database { .. }
            | SnapshotError::Open(_) => false,
            SnapshotError::ObjectTooLarge { .. }
            | SnapshotError::HashMismatch { .. }
            | SnapshotError::StateOpen(_)
            | SnapshotError::HistoryRead(_)
            | SnapshotError::Cancelled
            | SnapshotError::StateRead(_)
            | SnapshotError::FormatVersion { .. }
            | SnapshotError::IndexerBuild
            | SnapshotError::SpendingIndex { .. }
            | SnapshotError::NewerFormat { .. }
            | SnapshotError::NeedsMigration { .. }
            | SnapshotError::GenesisHeight
            | SnapshotError::EmptyState
            | SnapshotError::TipMismatch { .. }
            | SnapshotError::PrunedBelowWindow { .. }
            | SnapshotError::MissingColumnFamily(_)
            | SnapshotError::Inconsistent { .. } => false,
        }
    }
}
