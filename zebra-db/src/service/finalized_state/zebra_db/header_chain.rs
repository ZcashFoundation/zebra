//! The header chain: block headers stored ahead of their bodies (format 30.1.0).
//!
//! Headers-first sync validates headers and stores them before it downloads
//! the bodies. These headers must not change what the rest of Zebra sees as
//! the block chain, so they live in their own column families instead of
//! `block_header_by_height`, `hash_by_height` and `height_by_hash`:
//! - [`HEADER_BY_HEIGHT`]: `Height` → `block::Header`, the best header chain
//!   at heights above the finalized tip, and
//! - [`HEIGHT_BY_HEADER_HASH`]: `block::Hash` → `Height`, its reverse index.
//!
//! # Authoritative rows
//!
//! A row is authoritative above the finalized tip, and, in a database imported
//! from a state snapshot, below the snapshot window start `L`: the snapshot
//! carries no block below `L`, so the headers there are the node's only record
//! of those blocks, which it keeps without their bodies or block info.
//!
//! The block commit path never touches these column families, so any other row
//! at or below the finalized tip is a stale duplicate of a committed block.
//! Reads ignore stale rows, and the header writer deletes them when it starts,
//! before each batch, and every second while it is idle (see
//! [`DiskWriteBatch::prepare_header_chain_gc`]).
//!
//! The header chain tip is the last row above the finalized tip, or the
//! finalized tip when there are none. Above the finalized tip the header chain
//! is contiguous, and its lowest row links to the finalized tip, once the header
//! writer has collected stale rows: the block writer can finalize another
//! block just below a header batch the header writer is writing. A read can
//! also interleave with a header batch that replaces stored rows. So readers
//! that walk the header chain check that each header links to the one below
//! it.

use std::sync::Arc;

use zebra_chain::block::{self, Height};

use crate::{
    service::finalized_state::{
        disk_db::DiskWriteBatch, TypedColumnFamily, WriteTypedBatch, ZebraDb,
    },
    HashOrHeight,
};

/// The column family name: `Height` → `block::Header`.
pub const HEADER_BY_HEIGHT: &str = "header_by_height";

/// The column family name: `block::Hash` → `Height`.
pub const HEIGHT_BY_HEADER_HASH: &str = "height_by_header_hash";

/// The most stale rows one garbage collection deletes, so a write that follows
/// a long run of body commits doesn't stall the header writer. The header
/// writer also collects every second while it has no batch to commit.
pub const MAX_HEADER_GC_ROWS: usize = 10_000;

/// The type for reading the header chain's headers from the database.
pub type HeaderByHeightCf<'cf> = TypedColumnFamily<'cf, Height, block::Header>;

/// The type for reading the header chain's reverse index from the database.
pub type HeightByHeaderHashCf<'cf> = TypedColumnFamily<'cf, block::Hash, Height>;

impl ZebraDb {
    /// Returns a typed handle to the `header_by_height` column family.
    pub fn header_by_height_cf(&self) -> HeaderByHeightCf<'_> {
        HeaderByHeightCf::new(&self.db, HEADER_BY_HEIGHT)
            .expect("column family was created when database was created")
    }

    /// Returns a typed handle to the `height_by_header_hash` column family.
    pub fn height_by_header_hash_cf(&self) -> HeightByHeaderHashCf<'_> {
        HeightByHeaderHashCf::new(&self.db, HEIGHT_BY_HEADER_HASH)
            .expect("column family was created when database was created")
    }

    /// Returns `true` if a header chain row at `height` is authoritative: it is
    /// above the finalized tip, or below the window of a snapshot database.
    fn is_authoritative_header_height(&self, height: Height) -> bool {
        self.is_above_finalized_tip(height) || self.is_below_snapshot_window(height)
    }

    /// Returns `true` if `height` is above the finalized tip, or the database
    /// has no finalized tip.
    fn is_above_finalized_tip(&self, height: Height) -> bool {
        self.finalized_tip_height()
            .is_none_or(|finalized_tip| height > finalized_tip)
    }

    /// Returns `true` if `height` is above genesis and below the window start
    /// of the state snapshot this database was imported from.
    fn is_below_snapshot_window(&self, height: Height) -> bool {
        height > Height::MIN
            && self
                .snapshot_base()
                .is_some_and(|base| height < base.window_start)
    }

    /// The height and header of a block below the window of a snapshot
    /// database: the stored header, which the snapshot didn't replace with a
    /// block.
    ///
    /// `None` at any other height, and for heights the database has no header for.
    pub fn header_below_snapshot_window(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<(Height, Arc<block::Header>)> {
        let height = match hash_or_height {
            HashOrHeight::Height(height) => height,
            HashOrHeight::Hash(hash) => self.height_by_header_hash_cf().zs_get(&hash)?,
        };
        if !self.is_below_snapshot_window(height) {
            return None;
        }

        let header = self.header_by_height_cf().zs_get(&height)?;
        match hash_or_height {
            // A reverse-index row can outlive its header row, so check that both agree.
            HashOrHeight::Hash(hash) if header.hash() != hash => None,
            _ => Some((height, Arc::new(header))),
        }
    }

    /// The header of a known block: a finalized block, or a block below the
    /// window of a snapshot database, which has a header but no body.
    pub fn known_header(&self, hash_or_height: HashOrHeight) -> Option<Arc<block::Header>> {
        self.block_header(hash_or_height).or_else(|| {
            self.header_below_snapshot_window(hash_or_height)
                .map(|(_, header)| header)
        })
    }

    /// The tip of the stored header chain, if it is above the finalized tip.
    pub fn stored_header_tip(&self) -> Option<(Height, block::Hash)> {
        let (height, header) = self.header_by_height_cf().zs_last_key_value()?;

        self.is_above_finalized_tip(height)
            .then(|| (height, header.hash()))
    }

    /// The tip of the header chain: the stored header tip, or the finalized tip
    /// if no header is stored above it. `None` before genesis is committed.
    pub fn header_chain_tip(&self) -> Option<(Height, block::Hash)> {
        self.stored_header_tip().or_else(|| self.tip())
    }

    /// The header at `height` on the header chain: the finalized block's header
    /// at or below the finalized tip, or the stored header above it, or below
    /// the window of a snapshot database.
    pub fn any_header(&self, height: Height) -> Option<Arc<block::Header>> {
        if let Some(header) = self.block_header(height.into()) {
            return Some(header);
        }

        if !self.is_authoritative_header_height(height) {
            return None;
        }

        self.header_by_height_cf().zs_get(&height).map(Arc::new)
    }

    /// The hash at `height` on the header chain.
    pub fn header_chain_hash(&self, height: Height) -> Option<block::Hash> {
        if let Some(hash) = self.hash(height) {
            return Some(hash);
        }

        if !self.is_authoritative_header_height(height) {
            return None;
        }

        self.header_by_height_cf()
            .zs_get(&height)
            .map(|header| header.hash())
    }

    /// The height of `hash` on the header chain: a finalized block, or a
    /// stored header above the finalized tip, or below the window of a
    /// snapshot database.
    pub fn header_chain_height(&self, hash: block::Hash) -> Option<Height> {
        if let Some(height) = self.height(hash) {
            return Some(height);
        }

        let height = self.height_by_header_hash_cf().zs_get(&hash)?;

        // A reverse-index row can outlive its header row until the next
        // garbage collection, so check that both still agree.
        (self.is_authoritative_header_height(height)
            && self.header_chain_hash(height) == Some(hash))
        .then_some(height)
    }
}

impl DiskWriteBatch {
    /// Prepares `headers`, starting at `first_height`, as header chain rows in
    /// this batch.
    ///
    /// The caller has checked that the headers are contiguous, that they link
    /// to the header chain at `first_height - 1`, and that every stored row
    /// at or above `first_height` is deleted in the same batch.
    pub fn prepare_header_chain_append(
        &mut self,
        db: &ZebraDb,
        first_height: Height,
        headers: &[Arc<block::Header>],
    ) {
        let mut height = first_height;

        for header in headers {
            let hash = header.hash();

            let _ = self.header_rows(db).zs_insert(&height, header);
            let _ = self.reverse_header_rows(db).zs_insert(&hash, &height);

            height = height
                .next()
                .expect("header heights are checked against the maximum before commit");
        }
    }

    /// Prepares the deletion of every stored header chain row at or above
    /// `from` in this batch, and returns how many rows it deletes.
    pub fn prepare_header_chain_truncate(&mut self, db: &ZebraDb, from: Height) -> usize {
        let rows: Vec<(Height, block::Header)> = db
            .header_by_height_cf()
            .zs_forward_range_iter(from..)
            .collect();

        self.delete_header_rows(db, &rows)
    }

    /// Prepares the deletion of at most [`MAX_HEADER_GC_ROWS`] stale header
    /// chain rows, those at or below the finalized tip, in this batch, and
    /// returns how many rows it deletes.
    ///
    /// A snapshot database keeps the rows below its window start, which are
    /// not stale: they are its headers of the blocks the snapshot didn't carry.
    pub fn prepare_header_chain_gc(&mut self, db: &ZebraDb) -> usize {
        let Some(finalized_tip) = db.finalized_tip_height() else {
            return 0;
        };
        let floor = db
            .snapshot_base()
            .map_or(Height::MIN, |base| base.window_start);

        let rows: Vec<(Height, block::Header)> = db
            .header_by_height_cf()
            .zs_forward_range_iter(floor..=finalized_tip)
            .take(MAX_HEADER_GC_ROWS)
            .collect();

        self.delete_header_rows(db, &rows)
    }

    /// Prepares the deletion of `rows` and their reverse index rows in this
    /// batch, and returns how many rows it deletes.
    fn delete_header_rows(&mut self, db: &ZebraDb, rows: &[(Height, block::Header)]) -> usize {
        for (height, header) in rows {
            let _ = self.header_rows(db).zs_delete(height);
            let _ = self.reverse_header_rows(db).zs_delete(&header.hash());
        }

        rows.len()
    }

    /// Returns a typed writer for `header_by_height` rows in this batch.
    fn header_rows<'cf>(
        &'cf mut self,
        db: &'cf ZebraDb,
    ) -> WriteTypedBatch<'cf, Height, block::Header, &'cf mut DiskWriteBatch> {
        db.header_by_height_cf().with_batch_for_writing(self)
    }

    /// Returns a typed writer for `height_by_header_hash` rows in this batch.
    fn reverse_header_rows<'cf>(
        &'cf mut self,
        db: &'cf ZebraDb,
    ) -> WriteTypedBatch<'cf, block::Hash, Height, &'cf mut DiskWriteBatch> {
        db.height_by_header_hash_cf().with_batch_for_writing(self)
    }
}
