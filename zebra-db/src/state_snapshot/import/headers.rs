//! The headers above the snapshot height that the bootstrap fetched, checked
//! and stored in the imported database's header chain (design, "How a
//! snapshot node uses stored headers").
//!
//! The bootstrap fetches the headers `H + 1..=C` that chain the snapshot block
//! to the next checkpoint `C`, and checks their proof of work. The importer
//! checks their difficulty and time against the window headers, then writes
//! them to the header chain column families, so headers-first sync continues
//! from `C` instead of fetching them again.
//!
//! A node that synced its headers before it bootstrapped carries its whole
//! header chain into the staged database with [`carry_header_chain`], so it
//! keeps every header from genesis.

use std::{
    path::Path,
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use rocksdb::{WriteBatch, DB};

use zebra_chain::{block, parameters::Network};

use crate::{
    service::finalized_state::{
        disk_format::chain::SNAPSHOT_BASE,
        zebra_db::header_chain::{HEADER_BY_HEIGHT, HEIGHT_BY_HEADER_HASH},
        FromDisk, IntoDisk,
    },
    state_snapshot::format::ComponentId,
};

use super::{
    column_family, db_error, open_raw, refused, HeaderContextCheck, ImportCheck, SnapshotBase,
    SnapshotError,
};

/// How long [`carry_header_chain`] waits for the node to finish closing the synced state.
const SOURCE_CLOSE_TIMEOUT: Duration = Duration::from_secs(60);

/// How often [`carry_header_chain`] tries to open the synced state while the node closes it.
const SOURCE_CLOSE_RETRY: Duration = Duration::from_millis(100);

/// The most header rows [`carry_header_chain`] writes in one batch.
const CARRY_BATCH_HEADERS: usize = 10_000;

/// How many headers [`carry_header_chain`] carried into a staged snapshot database.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CarriedHeaders {
    /// The headers from height one to just below the snapshot window.
    pub below_window: u32,
    /// The headers above the snapshot height, including those the import
    /// already stored.
    pub above_snapshot: u32,
}

/// The most previous headers a header's difficulty and time context uses:
/// `PoWAveragingWindow + PoWMedianBlockSpan`, with room to spare.
const MAX_CONTEXT_HEADERS: usize = 64;

/// Checks `headers`, the headers from `H + 1` on, against the window headers
/// of the import database `db` and writes them to its header chain.
pub fn store_headers_above(
    db: &DB,
    base: &SnapshotBase,
    network: &Network,
    headers: &[Arc<block::Header>],
    check_header_context: HeaderContextCheck,
) -> Result<(), SnapshotError> {
    let fail = |detail: String| refused(ImportCheck::Headers, detail);
    if headers.is_empty() {
        return Ok(());
    }

    // The window headers, newest first, as the context of the first header above them.
    let mut context = Vec::with_capacity(MAX_CONTEXT_HEADERS);
    let mut iterator = db.raw_iterator_cf(column_family(db, ComponentId::Headers.column_family()));
    iterator.seek_for_prev(base.height.as_bytes());
    while iterator.valid() && context.len() < MAX_CONTEXT_HEADERS {
        let (key, value) = iterator
            .item()
            .expect("the iterator was checked to be valid");
        let height = block::Height::from_bytes(key);
        // Genesis is shipped too, and continues the window when the window starts at height 1.
        let continues_window = height >= base.window_start
            || (height.is_min() && base.window_start == block::Height(1));
        if !continues_window {
            break;
        }
        context.push(Arc::new(block::Header::from_bytes(value)));
        iterator.prev();
    }
    iterator
        .status()
        .map_err(db_error("iterating the import database"))?;
    drop(iterator);
    if context.first().map(|header| header.hash()) != Some(base.block_hash) {
        return Err(fail(
            "the window has no header for the snapshot block".to_string(),
        ));
    }

    let mut batch = WriteBatch::default();
    let header_cf = column_family(db, HEADER_BY_HEIGHT);
    let height_cf = column_family(db, HEIGHT_BY_HEADER_HASH);
    let mut height = base.height;
    for header in headers {
        height = height
            .next()
            .map_err(|_| fail("the headers run past the maximum height".to_string()))?;
        let parent = context[0].hash();
        if header.previous_block_hash != parent {
            return Err(fail(format!(
                "the header at {height:?} does not follow block {parent}"
            )));
        }
        check_header_context(&context, header, height, network).map_err(|error| {
            fail(format!(
                "the header at {height:?} has an invalid difficulty or time: {error}"
            ))
        })?;

        batch.put_cf(header_cf, height.as_bytes(), header.as_bytes());
        batch.put_cf(height_cf, header.hash().as_bytes(), height.as_bytes());

        context.insert(0, header.clone());
        context.truncate(MAX_CONTEXT_HEADERS);
    }

    db.write(batch)
        .map_err(db_error("writing the headers above the snapshot"))?;
    for cf in [header_cf, height_cf] {
        db.flush_cf(cf)
            .map_err(db_error("flushing the headers above the snapshot"))?;
    }
    Ok(())
}

/// Copies the header chain of the closed state database at `source` into the
/// staged snapshot database at `staged`, which [`import_state_snapshot`](super::import_state_snapshot)
/// built and checked, and returns how many headers it holds now.
///
/// `source` is the database of a node that synced headers from genesis before
/// it bootstrapped, and has committed no block above genesis.
///
/// - Every header below the snapshot window is copied. They must link genesis
///   to the first header of the window, or nothing is written: the staged
///   database knows no block below its window, or all of them.
/// - The headers above the snapshot height that continue the staged database's
///   headers are copied too, so headers-first sync continues where it was.
///
/// The window and the snapshot block come from the snapshot, which the source
/// headers are checked against, not the other way around.
pub fn carry_header_chain(source: &Path, staged: &Path) -> Result<CarriedHeaders, SnapshotError> {
    let unsupported = |detail: String| {
        SnapshotError::Unsupported(format!(
            "the synced headers in {source:?} can't be carried into the state snapshot: {detail}"
        ))
    };

    let source_db = open_closed(source)?;
    let staged_db = open_raw(staged)?;

    let base_bytes = staged_db
        .get_pinned_cf(column_family(&staged_db, SNAPSHOT_BASE), ().as_bytes())
        .map_err(db_error("reading the staged snapshot marker"))?
        .ok_or_else(|| unsupported("the staged database has no snapshot marker".to_string()))?;
    let base = SnapshotBase::from_bytes(base_bytes);
    let staged_header = |height: block::Height| -> Result<Option<block::Header>, SnapshotError> {
        let headers = column_family(&staged_db, ComponentId::Headers.column_family());
        Ok(staged_db
            .get_pinned_cf(headers, height.as_bytes())
            .map_err(db_error("reading the staged headers"))?
            .map(block::Header::from_bytes))
    };
    let window_header = staged_header(base.window_start)?
        .ok_or_else(|| unsupported("the staged database has no window headers".to_string()))?;
    let genesis_hash = staged_header(block::Height::MIN)?
        .ok_or_else(|| unsupported("the staged database has no genesis header".to_string()))?
        .hash();

    let source_headers = source_db.cf_handle(HEADER_BY_HEIGHT).ok_or_else(|| {
        unsupported("the synced state has no header chain column family".to_string())
    })?;
    let staged_headers = column_family(&staged_db, HEADER_BY_HEIGHT);
    let staged_heights = column_family(&staged_db, HEIGHT_BY_HEADER_HASH);
    let mut carried = CarriedHeaders::default();

    // Below the window: all of them, checked before any is written. Only their hashes are kept
    // between the passes, about 32 bytes for each of millions of headers.
    let mut hashes = Vec::new();
    let mut parent = genesis_hash;
    let mut rows = source_db.raw_iterator_cf(source_headers);
    rows.seek(block::Height(1).as_bytes());
    let mut height = block::Height(1);
    while height < base.window_start {
        let Some((key, value)) = rows.item() else {
            rows.status()
                .map_err(db_error("reading the synced headers"))?;
            return Err(unsupported(format!("there is no header at {height:?}")));
        };
        if block::Height::from_bytes(key) != height {
            return Err(unsupported(format!("there is no header at {height:?}")));
        }
        let header = block::Header::from_bytes(value);
        if header.previous_block_hash != parent {
            return Err(unsupported(format!(
                "the header at {height:?} does not follow block {parent}"
            )));
        }
        parent = header.hash();
        hashes.push(parent);
        let Ok(next) = height.next() else { break };
        height = next;
        rows.next();
    }
    if base.window_start > block::Height(1) && window_header.previous_block_hash != parent {
        return Err(unsupported(format!(
            "the synced header below the window is {parent}, but the snapshot's first window \
             header follows {}",
            window_header.previous_block_hash
        )));
    }

    rows.seek(block::Height(1).as_bytes());
    for chunk in hashes.chunks(CARRY_BATCH_HEADERS) {
        let mut batch = WriteBatch::default();
        for hash in chunk {
            let Some((key, value)) = rows.item() else {
                return Err(unsupported(
                    "the synced headers changed while they were copied".to_string(),
                ));
            };
            batch.put_cf(staged_headers, key, value);
            batch.put_cf(staged_heights, hash.as_bytes(), key);
            rows.next();
        }
        staged_db
            .write(batch)
            .map_err(db_error("writing the synced headers below the window"))?;
    }
    // The loop above checked a header at every height from one to just below the window.
    carried.below_window = base.window_start.0.saturating_sub(1);
    drop(hashes);

    // Above the snapshot: the source headers that continue the snapshot block and agree with the
    // headers the import stored.
    let mut parent = base.block_hash;
    let mut height = base
        .height
        .next()
        .map_err(|_| unsupported("the snapshot is at the maximum height".to_string()))?;
    rows.seek(height.as_bytes());
    let mut batch = WriteBatch::default();
    let mut pending = 0;
    while let Some((key, value)) = rows.item() {
        if block::Height::from_bytes(key) != height {
            break;
        }
        let header = block::Header::from_bytes(value);
        let hash = header.hash();
        let checked = staged_db
            .get_pinned_cf(staged_headers, height.as_bytes())
            .map_err(db_error("reading the stored headers above the snapshot"))?
            .map(block::Header::from_bytes);
        if header.previous_block_hash != parent
            || checked.is_some_and(|checked| checked.hash() != hash)
        {
            break;
        }
        batch.put_cf(staged_headers, height.as_bytes(), header.as_bytes());
        batch.put_cf(staged_heights, hash.as_bytes(), height.as_bytes());
        pending += 1;
        carried.above_snapshot += 1;
        if pending == CARRY_BATCH_HEADERS {
            staged_db
                .write(std::mem::take(&mut batch))
                .map_err(db_error("writing the synced headers above the snapshot"))?;
            pending = 0;
        }
        parent = hash;
        let Ok(next) = height.next() else { break };
        height = next;
        rows.next();
    }
    rows.status()
        .map_err(db_error("reading the synced headers"))?;
    staged_db
        .write(batch)
        .map_err(db_error("writing the synced headers above the snapshot"))?;

    for cf in [staged_headers, staged_heights] {
        staged_db
            .flush_cf(cf)
            .map_err(db_error("flushing the carried headers"))?;
    }

    Ok(carried)
}

/// Opens the database at `source`, which the node is closing, once it has closed it.
///
/// A database the process still has open can't be opened again, so this also
/// checks that nothing writes to it any more. Blocks for up to
/// [`SOURCE_CLOSE_TIMEOUT`].
fn open_closed(source: &Path) -> Result<DB, SnapshotError> {
    if !source.exists() {
        return Err(SnapshotError::Unsupported(format!(
            "there is no synced state at {source:?}"
        )));
    }
    let deadline = Instant::now() + SOURCE_CLOSE_TIMEOUT;
    loop {
        match open_raw(source) {
            Ok(db) => return Ok(db),
            Err(SnapshotError::Database { source, .. })
                if source.to_string().contains("lock") && Instant::now() < deadline =>
            {
                sleep(SOURCE_CLOSE_RETRY);
            }
            Err(error) => return Err(error),
        }
    }
}
