//! State snapshots: a pruned node's finalized state at a height `H`, exported
//! as content-addressed pieces that another node downloads and verifies.
//!
//! - [`format`] defines the manifest and the piece records;
//! - [`store`] keeps the verified objects on disk, by hash.

use std::{io, path::PathBuf};

use thiserror::Error;

use zebra_chain::state_snapshot::ObjectHash;

pub mod format;
pub mod store;

/// An error while storing or encoding a state snapshot.
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
}
