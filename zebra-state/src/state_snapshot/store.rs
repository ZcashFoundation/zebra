//! The artifact store (design, §4 "Artifact store"): the verified snapshot
//! objects this node holds, as one file per object named by its hash, and the
//! manifests it has rejected.
//!
//! Layout, under the state cache directory:
//!
//! ```text
//! state-snapshots/<network>/objects/<sha256-hex>
//! state-snapshots/<network>/rejected/<manifest-hex>
//! ```
//!
//! Objects are written to a temporary file, synced, and renamed into place,
//! so a file under `objects/` is always complete; it is still re-hashed
//! before it is trusted. Temporary files are hidden files in `objects/`,
//! whose names are never an object hash, so a reader that lists the
//! directory skips them by parsing each name as an [`ObjectHash`].

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use zebra_chain::{parameters::Network, state_snapshot::ObjectHash};

use crate::Config;

use super::{format::MAX_PIECE_BYTES, SnapshotError};

#[cfg(test)]
mod tests;

/// The largest object the store holds: a piece, which is larger than any
/// manifest. A larger file under `objects/` is never an object.
pub const MAX_OBJECT_BYTES: usize = MAX_PIECE_BYTES;

/// [`MAX_OBJECT_BYTES`] as a file length.
// `MAX_OBJECT_BYTES` is 16 MiB, which fits in a `u64`.
const MAX_OBJECT_LEN: u64 = MAX_OBJECT_BYTES as u64;

/// The directory under the state cache directory that holds every network's
/// store.
pub const STORE_DIR: &str = "state-snapshots";

/// The directory under a network's store that holds the objects.
pub const OBJECTS_DIR: &str = "objects";

/// The directory under a network's store that holds the rejection marks.
pub const REJECTED_DIR: &str = "rejected";

/// The snapshot objects this node holds for one network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactStore {
    /// The network's store directory.
    root: PathBuf,
}

impl ArtifactStore {
    /// Returns the store for `network` under the cache directory of the state
    /// `config`.
    pub fn new(config: &Config, network: &Network) -> Self {
        Self::at(
            config
                .cache_dir
                .join(STORE_DIR)
                .join(network.directory_name()),
        )
    }

    /// Returns the store rooted at `root`.
    pub fn at(root: PathBuf) -> Self {
        ArtifactStore { root }
    }

    /// Returns the store directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the directory that holds the objects.
    pub fn objects_dir(&self) -> PathBuf {
        self.root.join(OBJECTS_DIR)
    }

    /// Returns the path of the object `hash`, whether or not the store holds
    /// it.
    pub fn object_path(&self, hash: ObjectHash) -> PathBuf {
        self.objects_dir().join(hash.to_string())
    }

    /// Returns the directory that holds the rejection marks.
    pub fn rejected_dir(&self) -> PathBuf {
        self.root.join(REJECTED_DIR)
    }

    /// Returns the path of the rejection mark for `manifest`, whether or not
    /// it exists.
    pub fn rejected_path(&self, manifest: ObjectHash) -> PathBuf {
        self.rejected_dir().join(manifest.to_string())
    }

    /// Stores `object` as `hash`, which must be its hash, and returns its
    /// path. The file is complete when the method returns.
    ///
    /// The object is written to a temporary file in `objects/`, synced, and
    /// renamed over any previous copy, so the store never holds a partial
    /// object under its hash. Returns an error without writing anything if
    /// `hash` is not the object's hash, or the object is over
    /// [`MAX_OBJECT_BYTES`].
    pub fn put(&self, hash: ObjectHash, object: &[u8]) -> Result<PathBuf, SnapshotError> {
        if object.len() > MAX_OBJECT_BYTES {
            return Err(SnapshotError::ObjectTooLarge { len: object.len() });
        }
        let actual = ObjectHash::of(object);
        if actual != hash {
            return Err(SnapshotError::HashMismatch {
                expected: hash,
                actual,
            });
        }

        let dir = self.objects_dir();
        fs::create_dir_all(&dir).map_err(|source| io_error(&dir, source))?;

        // A hidden name that is never an object hash.
        let mut temp = tempfile::Builder::new()
            .prefix(&format!(".{hash}."))
            .suffix(".tmp")
            .tempfile_in(&dir)
            .map_err(|source| io_error(&dir, source))?;
        temp.write_all(object)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|source| io_error(temp.path(), source))?;

        let path = self.object_path(hash);
        temp.persist(&path)
            .map_err(|error| io_error(&path, error.error))?;
        sync_dir(&dir)?;

        Ok(path)
    }

    /// Returns whether the store holds `hash`, re-hashing the file before
    /// trusting it, so a file that was changed on disk is not served or
    /// imported.
    ///
    /// A missing file, something other than a file, a file over
    /// [`MAX_OBJECT_BYTES`], and a file with any other hash are all not held.
    /// Such a file is left in place: storing the object again replaces it.
    pub fn contains_verified(&self, hash: ObjectHash) -> Result<bool, SnapshotError> {
        let path = self.object_path(hash);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(io_error(&path, source)),
        };
        if !file
            .metadata()
            .map_err(|source| io_error(&path, source))?
            .is_file()
        {
            return Ok(false);
        }

        // Reading one byte past the limit is enough to tell an oversized file apart, so a large
        // file planted under an object name is never read in full.
        let mut hasher = Sha256::new();
        let len = io::copy(&mut file.take(MAX_OBJECT_LEN + 1), &mut hasher)
            .map_err(|source| io_error(&path, source))?;
        if len > MAX_OBJECT_LEN {
            return Ok(false);
        }

        Ok(ObjectHash(hasher.finalize().into()) == hash)
    }

    /// Reads up to `length` bytes of the object `hash` from `offset`, fewer
    /// at the end of the object.
    ///
    /// An `offset` at or past the end of the object reads nothing. Returns an
    /// [`io::ErrorKind::NotFound`] error if the store does not hold the
    /// object. The bytes are not re-hashed: callers serve objects they have
    /// verified with [`Self::contains_verified`], and nothing past
    /// [`MAX_OBJECT_BYTES`] is read, so a larger file planted under an object
    /// name is read as if it ended there.
    pub fn read_at(
        &self,
        hash: ObjectHash,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, SnapshotError> {
        let path = self.object_path(hash);
        let mut file = File::open(&path).map_err(|source| io_error(&path, source))?;
        let size = file
            .metadata()
            .map_err(|source| io_error(&path, source))?
            .len()
            .min(MAX_OBJECT_LEN);

        let len = size.saturating_sub(offset).min(u64::from(length));
        if len == 0 {
            return Ok(Vec::new());
        }

        file.seek(SeekFrom::Start(offset))
            .map_err(|source| io_error(&path, source))?;
        // `len` is at most `length`, a `u32`, which fits in a `usize` on supported targets.
        let mut data = Vec::with_capacity(len as usize);
        file.take(len)
            .read_to_end(&mut data)
            .map_err(|source| io_error(&path, source))?;

        Ok(data)
    }

    /// Returns the size of the object `hash`, or `None` if the store does not
    /// hold it.
    ///
    /// The size is the file's, which is not re-hashed.
    pub fn size(&self, hash: ObjectHash) -> Result<Option<u64>, SnapshotError> {
        let path = self.object_path(hash);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
            Ok(_not_a_file) => Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io_error(&path, source)),
        }
    }

    /// Marks the snapshot `manifest` as rejected: its contents failed the
    /// import checks, so it is never downloaded or served again.
    ///
    /// The mark is an empty file, synced before the method returns. Marking a
    /// manifest again is not an error.
    pub fn mark_rejected(&self, manifest: ObjectHash) -> Result<(), SnapshotError> {
        let dir = self.rejected_dir();
        fs::create_dir_all(&dir).map_err(|source| io_error(&dir, source))?;

        let path = self.rejected_path(manifest);
        File::create(&path)
            .and_then(|mark| mark.sync_all())
            .map_err(|source| io_error(&path, source))?;
        sync_dir(&dir)
    }

    /// Returns whether the snapshot `manifest` is marked as rejected.
    pub fn is_rejected(&self, manifest: ObjectHash) -> bool {
        self.rejected_path(manifest).is_file()
    }
}

/// Returns the error for the I/O operation on `path` that failed with `source`.
fn io_error(path: &Path, source: io::Error) -> SnapshotError {
    SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Syncs the directory `dir`, so the files renamed or created in it survive a
/// crash.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), SnapshotError> {
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|source| io_error(dir, source))
}

/// Directories cannot be opened as files on this platform, so their entries
/// are synced by the file system.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), SnapshotError> {
    Ok(())
}
