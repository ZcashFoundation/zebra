//! The headers above the snapshot height that the bootstrap fetched, checked
//! and stored in the imported database's header chain (design, "How a
//! snapshot node uses stored headers").
//!
//! The bootstrap fetches the headers `H + 1..=C` that chain the snapshot block
//! to the next checkpoint `C`, and checks their proof of work. The importer
//! checks their difficulty and time against the window headers, then writes
//! them to the header chain column families, so headers-first sync continues
//! from `C` instead of fetching them again.

use std::sync::Arc;

use rocksdb::{WriteBatch, DB};

use zebra_chain::{block, parameters::Network};

use crate::{
    service::{
        check::check_header_context,
        finalized_state::{HEADER_BY_HEIGHT, HEIGHT_BY_HEADER_HASH},
    },
    state_snapshot::format::ComponentId,
    FromDisk, IntoDisk,
};

use super::{column_family, db_error, refused, ImportCheck, SnapshotBase, SnapshotError};

/// The most previous headers a header's difficulty and time context uses:
/// `PoWAveragingWindow + PoWMedianBlockSpan`, with room to spare.
const MAX_CONTEXT_HEADERS: usize = 64;

/// Checks `headers`, the headers from `H + 1` on, against the window headers
/// of the import database `db` and writes them to its header chain.
pub(super) fn store_headers_above(
    db: &DB,
    base: &SnapshotBase,
    network: &Network,
    headers: &[Arc<block::Header>],
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
