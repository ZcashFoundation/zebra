//! Unit tests for the note-commitment-tree fetch lookahead scheduler and
//! bounded buffer.
//!
//! These exercise the pure scheduling/buffer bookkeeping without a live chain
//! or peers: the lookahead is bookkeeping over heights and byte counts.

use zebra_chain::block::Height;
use zebra_state::ShieldedPool;

use super::*;

/// A sapling fetch at `height`.
fn sapling(height: u32) -> TreeFetch {
    TreeFetch {
        height: Height(height),
        pool: ShieldedPool::Sapling,
    }
}

/// An orchard fetch at `height`.
fn orchard(height: u32) -> TreeFetch {
    TreeFetch {
        height: Height(height),
        pool: ShieldedPool::Orchard,
    }
}

#[test]
fn schedules_every_updating_height_ahead() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();

    // The chunk's sparse updating heights within the lookahead window: only
    // the heights that actually update a tree.
    let updates = vec![sapling(10), orchard(10), sapling(57), sapling(120)];

    let issued = lookahead.schedule(updates.clone());

    assert_eq!(
        issued, updates,
        "every not-yet-fetched updating height is scheduled, lowest first",
    );
    assert_eq!(lookahead.in_flight(), 4, "all four are now in flight");
}

#[test]
fn does_not_reissue_in_flight_or_buffered() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();

    // First pass schedules both pools at height 10 and sapling at 57.
    let first = lookahead.schedule(vec![sapling(10), orchard(10), sapling(57)]);
    assert_eq!(first.len(), 3);

    // Sapling@10 arrives and is buffered; orchard@10 and sapling@57 stay in
    // flight.
    lookahead.on_fetched(sapling(10), vec![0u8; 1024], Height(0));

    // A second pass over the same window must reissue nothing: sapling@10 is
    // buffered, the other two are still in flight.
    let second = lookahead.schedule(vec![sapling(10), orchard(10), sapling(57)]);
    assert!(
        second.is_empty(),
        "buffered and in-flight fetches are never reissued, got {second:?}",
    );

    assert_eq!(lookahead.buffered_heights(), 1);
    assert_eq!(lookahead.in_flight(), 2);
}

#[test]
fn take_returns_buffered_tree_then_falls_back() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();
    lookahead.schedule(vec![sapling(10), orchard(10)]);

    lookahead.on_fetched(sapling(10), vec![1u8; 2048], Height(0));
    lookahead.on_fetched(orchard(10), vec![2u8; 512], Height(0));

    assert_eq!(lookahead.buffer_bytes(), 2048 + 512);

    // The commit takes the buffered trees: the "tree supplied" path.
    let trees = lookahead.take(Height(10)).expect("trees were buffered");
    assert_eq!(trees.sapling.as_deref(), Some(&[1u8; 2048][..]));
    assert_eq!(trees.orchard.as_deref(), Some(&[2u8; 512][..]));

    // The buffer is emptied and its bytes released on take.
    assert_eq!(lookahead.buffered_heights(), 0);
    assert_eq!(lookahead.buffer_bytes(), 0);

    // A height with no buffered tree falls back to folding (None).
    assert!(
        lookahead.take(Height(11)).is_none(),
        "an un-fetched height returns None so the commit folds",
    );
}

#[test]
fn byte_cap_bounds_in_flight_issuance() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();

    // Before any tree has arrived the per-fetch reservation is the 64 KiB floor;
    // the byte cap therefore admits at most TREE_BUFFER_MAX_BYTES / 64 KiB
    // in-flight fetches.
    let max_in_flight = (TREE_BUFFER_MAX_BYTES / (64 * 1024)) as usize;

    // Offer far more updating heights than the byte cap allows in flight.
    let updates: Vec<TreeFetch> = (0..(max_in_flight as u32 + 100))
        .map(|h| sapling(h + 1))
        .collect();

    let issued = lookahead.schedule(updates);

    assert_eq!(
        issued.len(),
        max_in_flight,
        "the byte cap bounds the number of in-flight tree fetches",
    );
    assert_eq!(lookahead.in_flight(), max_in_flight);
}

#[test]
fn issuance_never_exceeds_either_cap() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();

    // Offer far more updating heights than either cap allows, in one pass.
    let updates: Vec<TreeFetch> = (0..(TREE_LOOKAHEAD_MAX + 10_000))
        .map(|h| sapling(h + 1))
        .collect();

    let issued = lookahead.schedule(updates);

    // Both bounds hold simultaneously: the count ceiling and the byte cap
    // (whichever binds first). With the 64 KiB per-fetch reservation floor the
    // byte cap binds first, but the test asserts both so neither can regress.
    let byte_cap_in_flight = (TREE_BUFFER_MAX_BYTES / (64 * 1024)) as usize;
    assert!(
        issued.len() <= TREE_LOOKAHEAD_MAX as usize,
        "issuance never exceeds the count ceiling, got {}",
        issued.len(),
    );
    assert_eq!(
        issued.len(),
        byte_cap_in_flight.min(TREE_LOOKAHEAD_MAX as usize),
        "issuance is bounded by whichever cap binds first",
    );
}

#[test]
fn evict_below_drops_passed_heights() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();
    lookahead.schedule(vec![sapling(5), sapling(20), sapling(40)]);

    lookahead.on_fetched(sapling(5), vec![0u8; 1000], Height(0));
    lookahead.on_fetched(sapling(20), vec![0u8; 1000], Height(0));
    // sapling(40) stays in flight.

    assert_eq!(lookahead.buffered_heights(), 2);
    assert_eq!(lookahead.in_flight(), 1);
    assert_eq!(lookahead.buffer_bytes(), 2000);

    // The frontier passes height 20: buffered tree at 5 and any in-flight below
    // 20 are evicted; the tree at 20 is kept (>= frontier), and in-flight 40
    // stays.
    lookahead.evict_below(Height(20));

    assert_eq!(
        lookahead.buffered_heights(),
        1,
        "only the buffered tree at or above the frontier is kept",
    );
    assert_eq!(lookahead.buffer_bytes(), 1000);
    assert_eq!(
        lookahead.in_flight(),
        1,
        "in-flight 40 is above the frontier"
    );

    // Taking the kept height still works.
    assert!(lookahead.take(Height(20)).is_some());
}

#[test]
fn late_arrival_below_frontier_is_dropped() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();
    lookahead.schedule(vec![sapling(10)]);

    // The block at height 10 already committed (folded) and the frontier moved
    // to 11 before the tree arrived: the late tree is dropped, not buffered.
    lookahead.on_fetched(sapling(10), vec![0u8; 4096], Height(11));

    assert_eq!(
        lookahead.buffered_heights(),
        0,
        "a tree below the frontier is dropped on arrival",
    );
    assert_eq!(lookahead.buffer_bytes(), 0);
    assert_eq!(lookahead.in_flight(), 0, "its in-flight marker is cleared");
}

#[test]
fn failed_fetch_can_be_reissued() {
    let _init = zebra_test::init();

    let mut lookahead = TreeLookahead::new();
    let issued = lookahead.schedule(vec![sapling(10)]);
    assert_eq!(issued, vec![sapling(10)]);
    assert_eq!(lookahead.in_flight(), 1);

    // The fetch failed: clear its in-flight marker.
    lookahead.on_failed(sapling(10));
    assert_eq!(lookahead.in_flight(), 0);

    // It is now reissuable.
    let reissued = lookahead.schedule(vec![sapling(10)]);
    assert_eq!(
        reissued,
        vec![sapling(10)],
        "a failed fetch is reissued on the next pass",
    );
}
