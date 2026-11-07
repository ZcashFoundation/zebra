//! Resumable `get-hashes` entry sweep, anchored against the spaced
//! checkpoint list.
//!
//! The sweep writes one flat, position-indexed file, so partial runs can be
//! resumed without any bookkeeping. The file holds one [`RECORD_BYTES`]
//! record per height, at offset `height * RECORD_BYTES`:
//! `hash[32] ‖ previous_block_hash[32] ‖ size u64 ‖ txs u64 ‖ txouts u64 ‖
//! notes u64`, the hashes in internal byte order and the counts
//! little-endian. An all-zero `hash` means "not fetched yet".
//!
//! Anchor verification: every swept hash at a height in the network's spaced
//! checkpoint list must match that list exactly.
//!
//! Link verification: the swept `previous_block_hash` at every height must be
//! the swept hash one height below (and the all-zero hash at genesis). This
//! chains every hash between two anchors to them, so a node that serves a
//! block from another chain, or a reorg between fetches, fails the run.
//!
//! A single mismatch of either kind fails the run, naming the height.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

use color_eyre::eyre::{bail, ensure, Result};
use tokio::task::JoinSet;

use zebra_chain::{
    block::{self, SyncHashEntry},
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
};

use crate::source::{BlockSource, SweptBlock};

/// The hash that marks a height as not fetched yet.
///
/// No real block hash is all zeroes: that value is reserved by the protocol
/// for "no previous block" in the genesis header, and producing it would
/// require a full SHA-256d preimage.
pub const UNFETCHED_HASH: [u8; 32] = [0; 32];

/// The size of one sweep record: two 32-byte hashes and four `u64` counts.
pub const RECORD_BYTES: usize = 2 * 32 + 4 * 8;

/// The offset of the counts in a sweep record.
const COUNTS_OFFSET: usize = 2 * 32;

/// How often sweep progress is reported, in completed heights.
const PROGRESS_INTERVAL: usize = 10_000;

/// The sweep output file, with its contents held in memory.
///
/// Writes go to both the in-memory copy and the backing file, so a killed
/// sweep loses at most the requests that were still in flight.
pub struct SweepFile {
    /// The backing file for `entries`.
    file: File,

    /// One record per height, with an [`UNFETCHED_HASH`] entry hash if the
    /// height has not been swept yet.
    entries: Vec<SweptBlock>,
}

impl SweepFile {
    /// Opens or creates the sweep file at `path`, and loads its contents.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        ensure!(
            bytes.len() % RECORD_BYTES == 0,
            "sweep file {} is corrupted: \
             its length {} is not a multiple of the {RECORD_BYTES}-byte record size",
            path.display(),
            bytes.len(),
        );

        let entries = bytes
            .chunks_exact(RECORD_BYTES)
            .map(decode_record)
            .collect();

        Ok(Self { file, entries })
    }

    /// Returns the swept record for `height`, or `None` if it has not been
    /// fetched yet.
    pub fn entry(&self, height: u32) -> Option<SweptBlock> {
        let swept = *self.entries.get(usize::try_from(height).ok()?)?;
        (swept.entry.hash.0 != UNFETCHED_HASH).then_some(swept)
    }

    /// Records the swept block for `height`, in memory and on disk.
    pub fn set_entry(&mut self, height: u32, swept: SweptBlock) -> Result<()> {
        ensure!(
            swept.entry.hash.0 != UNFETCHED_HASH,
            "height {height}: an all-zero block hash would be \
             indistinguishable from an unfetched entry",
        );

        let index = usize::try_from(height)?;
        if self.entries.len() <= index {
            self.entries.resize(index + 1, unfetched());
        }
        self.entries[index] = swept;

        // Writing past the end of the file extends it, leaving any gap zeroed,
        // which is exactly the "not fetched yet" record.
        //
        // `RECORD_BYTES` is 96, so it fits in a `u64`.
        self.file
            .seek(SeekFrom::Start(u64::from(height) * RECORD_BYTES as u64))?;
        self.file.write_all(&encode_record(&swept))?;

        Ok(())
    }

    /// Returns the entries for every height up to and including
    /// `max_height`, or an error naming the first missing height.
    pub fn complete_entries(&self, max_height: u32) -> Result<Vec<SyncHashEntry>> {
        let count = usize::try_from(u64::from(max_height) + 1)?;

        ensure!(
            self.entries.len() >= count,
            "the sweep file only covers heights below {}, \
             but --end-height is {max_height}: sweep the remaining range first",
            self.entries.len(),
        );

        if let Some(height) = self.entries[..count]
            .iter()
            .position(|swept| swept.entry.hash.0 == UNFETCHED_HASH)
        {
            bail!(
                "missing entry at height {height}: \
                 sweep the full range before --emit-pins"
            );
        }

        Ok(self.entries[..count]
            .iter()
            .map(|swept| swept.entry)
            .collect())
    }

    /// Flushes the backing file.
    pub fn flush(&mut self) -> Result<()> {
        self.file.flush()?;
        Ok(())
    }
}

/// An unfetched record.
fn unfetched() -> SweptBlock {
    SweptBlock {
        entry: SyncHashEntry {
            hash: block::Hash(UNFETCHED_HASH),
            size: 0,
            txs: 0,
            txouts: 0,
            notes: 0,
        },
        previous_block_hash: block::Hash(UNFETCHED_HASH),
    }
}

/// Encodes `swept` as a fixed-width sweep record.
fn encode_record(swept: &SweptBlock) -> [u8; RECORD_BYTES] {
    let entry = &swept.entry;
    let mut record = [0; RECORD_BYTES];

    record[..32].copy_from_slice(&entry.hash.0);
    record[32..COUNTS_OFFSET].copy_from_slice(&swept.previous_block_hash.0);
    for (slot, value) in record[COUNTS_OFFSET..].chunks_exact_mut(8).zip([
        entry.size,
        entry.txs,
        entry.txouts,
        entry.notes,
    ]) {
        slot.copy_from_slice(&value.to_le_bytes());
    }

    record
}

/// Decodes a fixed-width sweep record.
fn decode_record(record: &[u8]) -> SweptBlock {
    let word = |index: usize| {
        let start = COUNTS_OFFSET + 8 * index;
        u64::from_le_bytes(
            record[start..start + 8]
                .try_into()
                .expect("the slice is 8 bytes, because records are RECORD_BYTES long"),
        )
    };

    let hash = |start: usize| {
        block::Hash(
            record[start..start + 32]
                .try_into()
                .expect("the slice is 32 bytes, because records are RECORD_BYTES long"),
        )
    };

    SweptBlock {
        entry: SyncHashEntry {
            hash: hash(0),
            size: word(0),
            txs: word(1),
            txouts: word(2),
            notes: word(3),
        },
        previous_block_hash: hash(32),
    }
}

/// Sweeps the entry of every height in `start_height..=end_height` that is
/// missing from `file`, checking every fetched hash at a spaced checkpoint
/// height against `network`'s checkpoint list.
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
    file: &mut SweepFile,
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

    // The heights that still need a fetch.
    let work: Vec<u32> = (start_height..=end_height)
        .filter(|height| file.entry(*height).is_none())
        .collect();

    let total = work.len();
    eprintln!(
        "sweeping {total} of {} heights in {start_height}..={end_height} on {network}",
        u64::from(end_height) - u64::from(start_height) + 1,
    );

    let mut work = work.into_iter();
    let mut join_set: JoinSet<Result<(u32, SweptBlock)>> = JoinSet::new();
    let mut completed: usize = 0;
    let started = Instant::now();

    loop {
        // Keep up to `concurrency` per-height fetches in flight.
        while join_set.len() < concurrency {
            let Some(height) = work.next() else {
                break;
            };

            let source = source.clone();
            join_set.spawn(async move { Ok((height, source.swept_block(height).await?)) });
        }

        let Some(result) = join_set.join_next().await else {
            // No work left in flight, and the `while` above found none to
            // spawn: the sweep is finished.
            break;
        };
        let (height, swept) = result??;

        // Anchor verification: a freshly fetched hash at a spaced checkpoint
        // height must match the reviewed checkpoint list.
        if let Some(anchor) = checkpoints.hash(block::Height(height)) {
            ensure!(
                swept.entry.hash == anchor,
                "anchor mismatch at height {height}: \
                 the node returned {}, but the {network} checkpoint list \
                 pins {anchor}; the node is on the wrong chain or corrupted",
                swept.entry.hash,
            );
        }

        file.set_entry(height, swept)?;

        completed += 1;
        if completed.is_multiple_of(PROGRESS_INTERVAL) {
            // Casting to f64 can lose precision on huge counts, which is fine
            // for a progress rate display.
            let rate = completed as f64 / started.elapsed().as_secs_f64();
            eprintln!("swept {completed}/{total} heights ({rate:.0} heights/s)");
        }
    }

    file.flush()?;

    // Re-verify every covered anchor and link against the file contents,
    // which also covers entries resumed from previous runs, and links between
    // blocks fetched in different runs.
    let (verified, missing) = verify_anchors(network, file, end_height)?;
    let links = verify_links(file, end_height)?;
    eprintln!(
        "sweep complete: {completed} heights fetched, \
         {verified} checkpoint anchors verified, {missing} anchors not swept yet, \
         {links} parent links verified",
    );

    Ok(())
}

/// Checks every swept hash at a height in `network`'s spaced checkpoint list,
/// up to and including `end_height`, against that list.
///
/// Returns the number of verified anchors and the number of anchor heights
/// that have no swept entry yet (possible after a partial or restricted-range
/// sweep). Any mismatch is a hard error naming the height.
pub fn verify_anchors(
    network: &Network,
    file: &SweepFile,
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

        match file.entry(height.0) {
            Some(swept) => {
                ensure!(
                    swept.entry.hash == anchor,
                    "anchor mismatch at height {height:?}: \
                     the sweep file has {}, but the {network} checkpoint \
                     list pins {anchor}; re-sweep against a correctly synced node",
                    swept.entry.hash,
                );
                verified += 1;
            }
            None => missing += 1,
        }
    }

    Ok((verified, missing))
}

/// Checks that every swept block up to and including `end_height` is the
/// child of the swept block one height below it, and that the genesis block
/// has no parent.
///
/// Pairs with an unswept height are skipped, so this also works on a partial
/// sweep. Returns the number of verified links. Any mismatch is a hard error
/// naming the height.
pub fn verify_links(file: &SweepFile, end_height: u32) -> Result<usize> {
    let mut verified: usize = 0;

    if let Some(genesis) = file.entry(0) {
        ensure!(
            genesis.previous_block_hash == GENESIS_PREVIOUS_BLOCK_HASH,
            "link mismatch at height 0: the swept genesis block has parent {}; \
             re-sweep against a correctly synced node",
            genesis.previous_block_hash,
        );
        verified += 1;
    }

    for height in 1..=end_height {
        let (Some(parent), Some(child)) = (file.entry(height - 1), file.entry(height)) else {
            continue;
        };

        ensure!(
            child.previous_block_hash == parent.entry.hash,
            "link mismatch at height {height}: the swept block has parent {}, \
             but the swept hash at height {} is {}; the node served blocks \
             from different chains, or reorged during the sweep: \
             re-sweep against a correctly synced node",
            child.previous_block_hash,
            height - 1,
            parent.entry.hash,
        );
        verified += 1;
    }

    Ok(verified)
}
