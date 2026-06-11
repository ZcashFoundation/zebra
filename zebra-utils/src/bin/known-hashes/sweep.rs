//! Resumable block hash and size sweep, anchored against the spaced
//! checkpoint list.
//!
//! The sweep writes two flat, position-indexed files, so partial runs can be
//! resumed and merged without any bookkeeping:
//!
//! - the hash file holds one raw 32-byte internal-order block hash at offset
//!   `height * 32`, where all-zero bytes mean "not fetched yet", and
//! - the sizes file holds one little-endian `u32` serialized block size at
//!   offset `height * 4`, where `0` means "not fetched yet". This is the same
//!   format as existing `block-sizes-<network>.u32le` caches.
//!
//! Anchor verification: every swept hash at a height in the network's spaced
//! checkpoint list must match that list exactly — a single mismatch fails the
//! run, naming the height.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

use color_eyre::eyre::{bail, ensure, Result};
use tokio::task::JoinSet;

use zebra_chain::{block, parameters::Network};

use crate::source::BlockSource;

/// The hash file entry that marks a height as not fetched yet.
///
/// No real block hash is all zeroes: that value is reserved by the protocol
/// for "no previous block" in the genesis header, and producing it would
/// require a full SHA-256d preimage.
pub const UNFETCHED_HASH: [u8; 32] = [0; 32];

/// How often sweep progress is reported, in completed heights.
const PROGRESS_INTERVAL: usize = 10_000;

/// The two flat sweep output files, with their contents held in memory.
///
/// Writes go to both the in-memory copy and the backing file, so a killed
/// sweep loses at most the requests that were still in flight.
pub struct SweepFiles {
    /// The backing file for `hashes`.
    hash_file: File,

    /// One internal-order block hash per height, [`UNFETCHED_HASH`] if the
    /// height has not been swept yet.
    hashes: Vec<[u8; 32]>,

    /// The backing file for `sizes`.
    sizes_file: File,

    /// One serialized block size per height, `0` if the height has not been
    /// swept yet.
    sizes: Vec<u32>,
}

impl SweepFiles {
    /// Opens or creates the sweep files at `hash_path` and `sizes_path`, and
    /// loads their contents.
    pub fn open(hash_path: &Path, sizes_path: &Path) -> Result<Self> {
        let (hash_file, hash_bytes) = open_and_read(hash_path)?;
        ensure!(
            hash_bytes.len() % 32 == 0,
            "hash sweep file {} is corrupted: \
             its length {} is not a multiple of the 32-byte entry size",
            hash_path.display(),
            hash_bytes.len(),
        );

        let mut hashes = vec![UNFETCHED_HASH; hash_bytes.len() / 32];
        for (entry, chunk) in hashes.iter_mut().zip(hash_bytes.chunks_exact(32)) {
            entry.copy_from_slice(chunk);
        }

        let (sizes_file, size_bytes) = open_and_read(sizes_path)?;
        ensure!(
            size_bytes.len() % 4 == 0,
            "sizes file {} is corrupted: \
             its length {} is not a multiple of the 4-byte entry size",
            sizes_path.display(),
            size_bytes.len(),
        );

        let mut sizes = vec![0; size_bytes.len() / 4];
        for (entry, chunk) in sizes.iter_mut().zip(size_bytes.chunks_exact(4)) {
            let mut le_bytes = [0; 4];
            le_bytes.copy_from_slice(chunk);
            *entry = u32::from_le_bytes(le_bytes);
        }

        Ok(Self {
            hash_file,
            hashes,
            sizes_file,
            sizes,
        })
    }

    /// Returns the swept hash for `height`, or `None` if it has not been
    /// fetched yet.
    pub fn hash(&self, height: u32) -> Option<block::Hash> {
        let entry = *self.hashes.get(usize::try_from(height).ok()?)?;
        (entry != UNFETCHED_HASH).then_some(block::Hash(entry))
    }

    /// Returns the swept block size for `height`, or `None` if it has not
    /// been fetched yet.
    pub fn size(&self, height: u32) -> Option<u32> {
        let entry = *self.sizes.get(usize::try_from(height).ok()?)?;
        (entry != 0).then_some(entry)
    }

    /// Records the block hash for `height`, in memory and on disk.
    pub fn set_hash(&mut self, height: u32, hash: block::Hash) -> Result<()> {
        ensure!(
            hash.0 != UNFETCHED_HASH,
            "height {height}: an all-zero block hash would be \
             indistinguishable from an unfetched entry",
        );

        let index = usize::try_from(height)?;
        if self.hashes.len() <= index {
            self.hashes.resize(index + 1, UNFETCHED_HASH);
        }
        self.hashes[index] = hash.0;

        // Writing past the end of the file extends it, leaving any gap zeroed,
        // which is exactly the "not fetched yet" entry value.
        self.hash_file
            .seek(SeekFrom::Start(u64::from(height) * 32))?;
        self.hash_file.write_all(&hash.0)?;

        Ok(())
    }

    /// Records the serialized block size for `height`, in memory and on disk.
    pub fn set_size(&mut self, height: u32, size: u32) -> Result<()> {
        ensure!(
            size != 0,
            "height {height}: a zero block size would be \
             indistinguishable from an unfetched entry",
        );

        let index = usize::try_from(height)?;
        if self.sizes.len() <= index {
            self.sizes.resize(index + 1, 0);
        }
        self.sizes[index] = size;

        // As in `set_hash`, any gap left by extending the file is zeroed,
        // which is the "not fetched yet" entry value.
        self.sizes_file
            .seek(SeekFrom::Start(u64::from(height) * 4))?;
        self.sizes_file.write_all(&size.to_le_bytes())?;

        Ok(())
    }

    /// Returns the hashes for every height up to and including `max_height`,
    /// or an error naming the first missing height.
    pub fn complete_hashes(&self, max_height: u32) -> Result<Vec<[u8; 32]>> {
        let count = usize::try_from(u64::from(max_height) + 1)?;

        ensure!(
            self.hashes.len() >= count,
            "hash sweep file only covers heights below {}, \
             but --end-height is {max_height}: sweep the remaining range first",
            self.hashes.len(),
        );

        if let Some(height) = self.hashes[..count]
            .iter()
            .position(|entry| *entry == UNFETCHED_HASH)
        {
            bail!(
                "missing block hash at height {height}: \
                 sweep the full range before --emit-chunks"
            );
        }

        Ok(self.hashes[..count].to_vec())
    }

    /// Returns the block sizes for every height up to and including
    /// `max_height`, or an error naming the first missing height.
    pub fn complete_sizes(&self, max_height: u32) -> Result<Vec<u32>> {
        let count = usize::try_from(u64::from(max_height) + 1)?;

        ensure!(
            self.sizes.len() >= count,
            "sizes file only covers heights below {}, \
             but --end-height is {max_height}: sweep the remaining range first",
            self.sizes.len(),
        );

        if let Some(height) = self.sizes[..count].iter().position(|entry| *entry == 0) {
            bail!(
                "missing block size at height {height}: \
                 sweep the full range before --emit-chunks"
            );
        }

        Ok(self.sizes[..count].to_vec())
    }

    /// Flushes both backing files.
    pub fn flush(&mut self) -> Result<()> {
        self.hash_file.flush()?;
        self.sizes_file.flush()?;
        Ok(())
    }
}

/// Opens or creates the file at `path` for reading and writing, and reads its
/// full contents.
fn open_and_read(path: &Path) -> Result<(File, Vec<u8>)> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;

    Ok((file, bytes))
}

/// Sweeps the block hash and size of every height in
/// `start_height..=end_height` that is missing from `files`, checking every
/// fetched hash at a spaced checkpoint height against `network`'s checkpoint
/// list.
///
/// Runs up to `concurrency` requests against `source` in parallel, and
/// records each result as soon as it arrives, so an interrupted sweep resumes
/// where it left off.
//
// Progress reporting goes to stderr because this is an interactive
// maintenance tool, and `init_tracing()` does not install a format layer.
#[allow(clippy::print_stderr)]
pub async fn run_sweep<S: BlockSource>(
    source: &S,
    network: &Network,
    files: &mut SweepFiles,
    start_height: u32,
    end_height: u32,
    concurrency: usize,
) -> Result<()> {
    ensure!(
        start_height <= end_height,
        "--start-height {start_height} is above --end-height {end_height}",
    );
    ensure!(concurrency > 0, "--concurrency must be at least 1");

    let checkpoints = network.checkpoint_list();

    // The heights that still need a hash or size fetch.
    let work: Vec<(u32, bool, bool)> = (start_height..=end_height)
        .filter_map(|height| {
            let need_hash = files.hash(height).is_none();
            let need_size = files.size(height).is_none();
            (need_hash || need_size).then_some((height, need_hash, need_size))
        })
        .collect();

    let total = work.len();
    eprintln!(
        "sweeping {total} of {} heights in {start_height}..={end_height} on {network}",
        u64::from(end_height) - u64::from(start_height) + 1,
    );

    let mut work = work.into_iter();
    let mut join_set: JoinSet<Result<(u32, Option<block::Hash>, Option<u32>)>> = JoinSet::new();
    let mut completed: usize = 0;
    let started = Instant::now();

    loop {
        // Keep up to `concurrency` per-height fetches in flight.
        while join_set.len() < concurrency {
            let Some((height, need_hash, need_size)) = work.next() else {
                break;
            };

            let source = source.clone();
            join_set.spawn(async move {
                let hash = if need_hash {
                    Some(source.block_hash(height).await?)
                } else {
                    None
                };
                let size = if need_size {
                    Some(source.block_size(height).await?)
                } else {
                    None
                };

                Ok((height, hash, size))
            });
        }

        let Some(result) = join_set.join_next().await else {
            // No work left in flight, and the `while` above found none to
            // spawn: the sweep is finished.
            break;
        };
        let (height, hash, size) = result??;

        if let Some(hash) = hash {
            // Anchor verification: a freshly fetched hash at a spaced
            // checkpoint height must match the reviewed checkpoint list.
            if let Some(anchor) = checkpoints.hash(block::Height(height)) {
                ensure!(
                    hash == anchor,
                    "anchor mismatch at height {height}: \
                     the node returned {hash}, but the {network} checkpoint list \
                     pins {anchor}; the node is on the wrong chain or corrupted",
                );
            }

            files.set_hash(height, hash)?;
        }

        if let Some(size) = size {
            files.set_size(height, size)?;
        }

        completed += 1;
        if completed % PROGRESS_INTERVAL == 0 {
            // Casting to f64 can lose precision on huge counts, which is fine
            // for a progress rate display.
            let rate = completed as f64 / started.elapsed().as_secs_f64();
            eprintln!("swept {completed}/{total} heights ({rate:.0} heights/s)");
        }
    }

    files.flush()?;

    // Re-verify every covered anchor against the file contents, which also
    // covers entries resumed from previous runs.
    let (verified, missing) = verify_anchors(network, files, end_height)?;
    eprintln!(
        "sweep complete: {completed} heights fetched, \
         {verified} checkpoint anchors verified, {missing} anchors not swept yet",
    );

    Ok(())
}

/// Checks every swept hash at a height in `network`'s spaced checkpoint list,
/// up to and including `end_height`, against that list.
///
/// Returns the number of verified anchors and the number of anchor heights
/// that have no swept hash yet (possible after a partial or restricted-range
/// sweep). Any mismatch is a hard error naming the height.
pub fn verify_anchors(
    network: &Network,
    files: &SweepFiles,
    end_height: u32,
) -> Result<(usize, usize)> {
    let checkpoints = network.checkpoint_list();
    let mut verified: usize = 0;
    let mut missing: usize = 0;

    for (height, anchor) in checkpoints.iter_cloned() {
        if height.0 > end_height {
            // `iter_cloned()` returns checkpoints in increasing height order.
            break;
        }

        match files.hash(height.0) {
            Some(swept) => {
                ensure!(
                    swept == anchor,
                    "anchor mismatch at height {height:?}: \
                     the swept hash file has {swept}, but the {network} checkpoint \
                     list pins {anchor}; re-sweep against a correctly synced node",
                );
                verified += 1;
            }
            None => missing += 1,
        }
    }

    Ok((verified, missing))
}
