//! Unit tests for the discovery-fed [`HashSource`].

use futures::FutureExt;

use zebra_chain::block;

use super::{DiscoverySource, DISCOVERY_LOW_WATER_MARK};
use crate::components::ibd::engine::HashSource;

/// Returns a distinct test hash for `id`.
fn hash(id: u8) -> block::Hash {
    block::Hash([id; 32])
}

/// Drains queued feed events into the source, as the engine's refill pass does.
fn drain(source: &mut DiscoverySource) {
    source
        .ensure_covers(block::Height(0), block::Height(0))
        .now_or_never()
        .expect("ensure_covers never waits")
        .expect("ensure_covers never fails");
}

#[test]
fn extend_grows_the_range_and_serves_hashes_by_height() {
    let base = block::Height(100);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(99));

    // With no hashes, the range is empty: the maximum is just below the base.
    assert_eq!(source.max_height(), block::Height(99));

    feed.extend(vec![hash(1), hash(2), hash(3)]);
    drain(&mut source);

    assert_eq!(source.max_height(), block::Height(102));
    assert_eq!(source.hash(block::Height(100)).unwrap(), Some(hash(1)));
    assert_eq!(source.hash(block::Height(102)).unwrap(), Some(hash(3)));

    // Just below the base: the anchor, serving the frontier's parent pin.
    assert_eq!(source.hash(block::Height(99)).unwrap(), Some(hash(99)));
    // Below the anchor: already committed and released.
    assert_eq!(source.hash(block::Height(98)).unwrap(), None);
    // Above the discovered range: not known yet.
    assert_eq!(source.hash(block::Height(103)).unwrap(), None);
}

#[test]
fn extend_skips_an_overlapping_first_hash() {
    let base = block::Height(10);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(9));

    feed.extend(vec![hash(1), hash(2)]);
    // A crawl overlap: the next response starts with the current tail.
    feed.extend(vec![hash(2), hash(3)]);
    drain(&mut source);

    assert_eq!(source.max_height(), block::Height(12));
    assert_eq!(source.hash(block::Height(11)).unwrap(), Some(hash(2)));
    assert_eq!(source.hash(block::Height(12)).unwrap(), Some(hash(3)));
}

#[test]
fn release_below_advances_the_base_and_anchor() {
    let base = block::Height(5);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(4));

    // The anchor serves the initial frontier's parent pin.
    assert_eq!(source.hash(block::Height(4)).unwrap(), Some(hash(4)));

    feed.extend(vec![hash(1), hash(2), hash(3)]);
    drain(&mut source);

    source.release_below(block::Height(7));

    assert_eq!(source.hash(block::Height(5)).unwrap(), None);
    // The last released hash becomes the anchor for the new base.
    assert_eq!(source.hash(block::Height(6)).unwrap(), Some(hash(2)));
    assert_eq!(source.hash(block::Height(7)).unwrap(), Some(hash(3)));
    assert_eq!(source.max_height(), block::Height(7));
}

#[test]
fn invalidate_above_truncates_the_range() {
    let base = block::Height(1);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(255));

    feed.extend(vec![hash(1), hash(2), hash(3), hash(4)]);
    drain(&mut source);

    source.invalidate_above(block::Height(2));

    assert_eq!(source.max_height(), block::Height(2));
    assert_eq!(source.hash(block::Height(2)).unwrap(), Some(hash(2)));
    assert_eq!(source.hash(block::Height(3)).unwrap(), None);
}

#[tokio::test]
async fn wait_for_growth_reports_queued_and_awaited_extends() {
    let base = block::Height(1);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(255));

    // Growth already queued: resolves immediately.
    feed.extend(vec![hash(1)]);
    assert!(source.wait_for_growth().await);
    assert!(!source.is_final());

    // Nothing queued above the current maximum: pending until the feed extends.
    let mut growth = source.wait_for_growth();
    assert!(growth.as_mut().now_or_never().is_none());
    feed.extend(vec![hash(2)]);
    assert!(growth.await);
}

/// The engine retains the committed parent as `hashes[0]` via
/// `release_below(base - 1)`; a fully-committed source must still report *no*
/// growth (pending), or the engine's completion loop spins.
#[tokio::test]
async fn wait_for_growth_pends_after_the_whole_range_is_committed() {
    let base = block::Height(10);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(9));

    feed.extend(vec![hash(1), hash(2)]);
    drain(&mut source);

    // The engine commits heights 10 and 11, then advances its base to 12 and
    // calls `release_below(11)` — retaining the committed parent at height 11.
    source.release_below(block::Height(11));
    assert_eq!(source.max_height(), block::Height(11));

    // Every fed hash is committed and the crawl has not finished: no growth.
    let mut growth = source.wait_for_growth();
    assert!(
        growth.as_mut().now_or_never().is_none(),
        "a drained-but-not-final source must pend, not report the retained parent as growth",
    );

    // A fresh hash is real growth.
    feed.extend(vec![hash(3)]);
    assert!(growth.await);
}

#[tokio::test]
async fn finish_completes_an_empty_source() {
    let base = block::Height(1);
    let (feed, mut source) = DiscoverySource::new(base, hash(255));

    feed.finish();
    assert!(!source.wait_for_growth().await);
    assert!(source.is_final());
}

#[tokio::test]
async fn dropping_the_feed_finishes_the_source() {
    let base = block::Height(1);
    let (feed, mut source) = DiscoverySource::new(base, hash(255));
    drop(feed);

    assert!(!source.wait_for_growth().await);
    assert!(source.is_final());
}

#[tokio::test]
async fn a_finish_behind_queued_hashes_still_reports_growth() {
    let base = block::Height(1);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(255));

    feed.extend(vec![hash(1)]);
    feed.finish();

    // The queued hashes are still fetchable growth; the finish only marks the
    // range final.
    assert!(source.wait_for_growth().await);
    assert!(source.is_final());
    assert_eq!(source.hash(block::Height(1)).unwrap(), Some(hash(1)));
}

/// A crawl-overlap skip counts in the feed's `fed` total, so the source must
/// account for it in `released` too — otherwise `fed - released` over-counts
/// and, after enough skips, `wants_more_hashes` would stop crawling while the
/// engine still waits for hashes, deadlocking the cycle.
#[tokio::test]
async fn overlap_skips_do_not_inflate_the_outstanding_count() {
    let base = block::Height(1);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(255));

    // Feed just over the low-water mark, then repeatedly feed a 1-hash overlap
    // (the tail) followed by nothing new. Each overlap is counted in `fed` but
    // skipped by the source.
    let above_mark = DISCOVERY_LOW_WATER_MARK + 1;
    feed.extend((1..=above_mark).map(|id| hash(id as u8)).collect());
    drain(&mut source);

    let tail = hash(above_mark as u8);
    for _ in 0..50 {
        // A pure-overlap round: the only hash equals the current tail.
        feed.extend(vec![tail]);
        drain(&mut source);
    }

    // Commit a few blocks. With the skips accounted for in `released`, the
    // outstanding count is now under the mark and `wants_more_hashes`
    // resolves; without the fix the 50 skips keep it above the mark and this
    // would hang.
    source.release_below(block::Height(10));
    feed.wants_more_hashes().await;
}

#[tokio::test]
async fn wants_more_hashes_follows_the_outstanding_low_water_mark() {
    let base = block::Height(1);
    let (mut feed, mut source) = DiscoverySource::new(base, hash(255));

    // An empty source always wants hashes.
    feed.wants_more_hashes().await;

    // Feed well past the low-water mark. The feed tracks the fed count itself,
    // so it wants no more *without* the engine having drained anything yet.
    let above_mark = DISCOVERY_LOW_WATER_MARK + 50;
    feed.extend((0..above_mark).map(|id| hash(id as u8)).collect());
    assert!(feed.wants_more_hashes().now_or_never().is_none());

    // Releasing (committing) down to the mark asks for more. The source must
    // hold the hashes to release them, so drain the feed into it first.
    drain(&mut source);
    source.release_below(block::Height(1 + 51));
    feed.wants_more_hashes().await;

    // A gone source (engine finished or failed) never blocks the syncer.
    feed.extend(
        (0..above_mark)
            .map(|id| hash(id.wrapping_add(100) as u8))
            .collect(),
    );
    drop(source);
    feed.wants_more_hashes().await;
}
