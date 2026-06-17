//! Shared Zakura sync frontier contract.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use futures::future::BoxFuture;
use serde_json::{Number, Value};
use tokio::sync::watch;
use zebra_chain::block;

use super::{
    commit_state_trace as cs_trace, BlockApplyResult, BlockApplyToken, BlockSyncBlockMeta,
    HeaderSyncCommitFailureKind, ZakuraPeerId, ZakuraTrace, COMMIT_STATE_TABLE,
};

/// A height/hash pair at one chain frontier.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Frontier {
    /// Frontier height.
    pub height: block::Height,
    /// Frontier block hash.
    pub hash: block::Hash,
}

impl Frontier {
    /// Construct a frontier from its height and hash.
    pub fn new(height: block::Height, hash: block::Hash) -> Self {
        Self { height, hash }
    }
}

/// Shared Zakura chain facts owned by the sync exchange.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ChainFrontier {
    /// Highest finalized block.
    pub finalized: Frontier,
    /// Highest verified block body.
    pub verified_body: Frontier,
    /// Highest committed header target.
    pub best_header: Frontier,
}

/// Cause for a shared frontier update.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrontierChange {
    /// Initial or whole-state snapshot.
    Snapshot,
    /// Verified body frontier advanced.
    VerifiedGrow,
    /// Verified body frontier was reset, possibly lower.
    VerifiedReset,
    /// Best header target advanced.
    HeaderAdvanced,
    /// Best header target was reanchored, possibly lower.
    HeaderReanchored,
}

/// Latest shared frontier plus the transition cause.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FrontierUpdate {
    /// Current shared frontier after the change.
    pub frontier: ChainFrontier,
    /// Cause of the current value.
    pub change: FrontierChange,
}

/// Single owner of shared Zakura sync frontiers.
#[derive(Clone, Debug)]
pub struct ZakuraSyncExchange {
    inner: Arc<ZakuraSyncExchangeInner>,
}

#[derive(Debug)]
struct ZakuraSyncExchangeInner {
    frontier: watch::Sender<FrontierUpdate>,
    trace: ZakuraTrace,
    sequence: AtomicU64,
}

impl ZakuraSyncExchange {
    /// Create a shared sync exchange with an initial coherent frontier snapshot.
    pub fn new(initial: FrontierUpdate, trace: ZakuraTrace) -> Self {
        let (frontier, _receiver) = watch::channel(initial);

        Self {
            inner: Arc::new(ZakuraSyncExchangeInner {
                frontier,
                trace,
                sequence: AtomicU64::new(0),
            }),
        }
    }

    /// Return the currently cached shared frontier update.
    pub fn current_frontier(&self) -> FrontierUpdate {
        *self.inner.frontier.borrow()
    }

    /// Subscribe to latest-value frontier updates.
    pub fn subscribe_frontier(&self) -> watch::Receiver<FrontierUpdate> {
        self.inner.frontier.subscribe()
    }

    /// Publish a candidate frontier update from `source`.
    pub fn publish_frontier(&self, requested: FrontierUpdate, source: &'static str) {
        let mut transition = None;

        self.inner.frontier.send_if_modified(|current| {
            let old = *current;
            let sequence = self
                .inner
                .sequence
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);

            match apply_frontier_update(old, requested) {
                Some(update) => {
                    *current = update;
                    transition = Some((sequence, old, update, "accepted"));
                    true
                }
                None => {
                    let ignored = FrontierUpdate {
                        frontier: old.frontier,
                        change: requested.change,
                    };
                    transition = Some((sequence, old, ignored, "ignored"));
                    false
                }
            }
        });

        if let Some((sequence, old, new, result)) = transition {
            self.trace_transition(sequence, source, old, new, result);
        }
    }

    fn trace_transition(
        &self,
        sequence: u64,
        source: &'static str,
        old: FrontierUpdate,
        new: FrontierUpdate,
        result: &'static str,
    ) {
        self.inner.trace.emit_with(COMMIT_STATE_TABLE, |row| {
            row.insert(
                cs_trace::EVENT.to_string(),
                Value::String(cs_trace::SYNC_FRONTIER_TRANSITION.to_string()),
            );
            row.insert(
                cs_trace::SOURCE.to_string(),
                Value::String(source.to_string()),
            );
            row.insert(
                cs_trace::SEQUENCE.to_string(),
                Value::Number(Number::from(sequence)),
            );
            row.insert(
                cs_trace::CAUSE.to_string(),
                Value::String(frontier_change_label(new.change).to_string()),
            );
            row.insert(
                cs_trace::RESULT.to_string(),
                Value::String(result.to_string()),
            );
            insert_frontier_fields(row, "old", old.frontier);
            insert_frontier_fields(row, "new", new.frontier);
        });
    }
}

/// Result of a header range commit through the sync exchange.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HeaderRangeCommit {
    /// First committed header height.
    pub start_height: block::Height,
    /// New durable best header frontier.
    pub tip: Frontier,
}

/// Result of a submitted block body through the sync exchange.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlockBodySubmit {
    /// Submission token echoed from the reactor.
    pub token: BlockApplyToken,
    /// Submitted block height.
    pub height: block::Height,
    /// Submitted block hash.
    pub hash: block::Hash,
    /// Verifier result.
    pub result: BlockApplyResult,
    /// Locally observed shared frontier after the apply attempt.
    pub local_frontier: Option<ChainFrontier>,
}

/// Header-sync view of shared Zakura state.
pub trait HeaderSyncStatePortImpl: Send + Sync + 'static {
    /// Return the currently cached shared frontier.
    fn current_frontier(&self) -> ChainFrontier;

    /// Subscribe to latest-value shared frontier updates.
    fn subscribe_frontier(&self) -> watch::Receiver<FrontierUpdate>;

    /// Commit a contiguous header range.
    fn commit_header_range(
        &self,
        peer: ZakuraPeerId,
        anchor: block::Hash,
        start_height: block::Height,
        headers: Vec<Arc<block::Header>>,
        body_sizes: Vec<u32>,
        finalized: bool,
    ) -> BoxFuture<'static, Result<HeaderRangeCommit, HeaderSyncCommitFailureKind>>;

    /// Publish a locally accepted best-header advance.
    fn publish_best_header(&self, tip: Frontier) -> BoxFuture<'static, ()>;

    /// Publish a best-header reanchor.
    fn publish_header_reanchor(&self, old: Frontier, new: Frontier) -> BoxFuture<'static, ()>;
}

/// Block-sync view of shared Zakura state.
pub trait BlockSyncStatePortImpl: Send + Sync + 'static {
    /// Return the currently cached shared frontier.
    fn current_frontier(&self) -> ChainFrontier;

    /// Subscribe to latest-value shared frontier updates.
    fn subscribe_frontier(&self) -> watch::Receiver<FrontierUpdate>;

    /// Query committed headers that still need block bodies.
    fn query_missing_bodies(
        &self,
        verified_body: block::Height,
        best_header: block::Height,
    ) -> BoxFuture<'static, Result<Vec<BlockSyncBlockMeta>, zebra_chain::BoxError>>;

    /// Submit a downloaded body to the verifier/state pipeline.
    fn submit_block_body(
        &self,
        token: BlockApplyToken,
        block: Arc<block::Block>,
    ) -> BoxFuture<'static, BlockBodySubmit>;

    /// Read committed blocks for serving stream-6 peers.
    fn read_committed_blocks(
        &self,
        start: block::Height,
        count: u32,
    ) -> BoxFuture<
        'static,
        Result<Vec<(block::Height, Arc<block::Block>, usize)>, zebra_chain::BoxError>,
    >;

    /// Publish body-sync progress that did not come from a direct submit path.
    fn publish_body_progress(&self, update: FrontierUpdate) -> BoxFuture<'static, ()>;
}

/// Cloneable header-sync state port.
#[derive(Clone)]
pub struct HeaderSyncStatePort {
    inner: Arc<dyn HeaderSyncStatePortImpl>,
}

impl HeaderSyncStatePort {
    /// Wrap an implementation in a cloneable port.
    pub fn new(inner: Arc<dyn HeaderSyncStatePortImpl>) -> Self {
        Self { inner }
    }

    /// Return the currently cached shared frontier.
    pub fn current_frontier(&self) -> ChainFrontier {
        self.inner.current_frontier()
    }

    /// Subscribe to latest-value shared frontier updates.
    pub fn subscribe_frontier(&self) -> watch::Receiver<FrontierUpdate> {
        self.inner.subscribe_frontier()
    }

    /// Commit a contiguous header range.
    pub fn commit_header_range(
        &self,
        peer: ZakuraPeerId,
        anchor: block::Hash,
        start_height: block::Height,
        headers: Vec<Arc<block::Header>>,
        body_sizes: Vec<u32>,
        finalized: bool,
    ) -> BoxFuture<'static, Result<HeaderRangeCommit, HeaderSyncCommitFailureKind>> {
        self.inner
            .commit_header_range(peer, anchor, start_height, headers, body_sizes, finalized)
    }

    /// Publish a locally accepted best-header advance.
    pub fn publish_best_header(&self, tip: Frontier) -> BoxFuture<'static, ()> {
        self.inner.publish_best_header(tip)
    }

    /// Publish a best-header reanchor.
    pub fn publish_header_reanchor(&self, old: Frontier, new: Frontier) -> BoxFuture<'static, ()> {
        self.inner.publish_header_reanchor(old, new)
    }
}

impl std::fmt::Debug for HeaderSyncStatePort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HeaderSyncStatePort")
    }
}

/// Cloneable block-sync state port.
#[derive(Clone)]
pub struct BlockSyncStatePort {
    inner: Arc<dyn BlockSyncStatePortImpl>,
}

impl BlockSyncStatePort {
    /// Wrap an implementation in a cloneable port.
    pub fn new(inner: Arc<dyn BlockSyncStatePortImpl>) -> Self {
        Self { inner }
    }

    /// Return the currently cached shared frontier.
    pub fn current_frontier(&self) -> ChainFrontier {
        self.inner.current_frontier()
    }

    /// Subscribe to latest-value shared frontier updates.
    pub fn subscribe_frontier(&self) -> watch::Receiver<FrontierUpdate> {
        self.inner.subscribe_frontier()
    }

    /// Query committed headers that still need block bodies.
    pub fn query_missing_bodies(
        &self,
        verified_body: block::Height,
        best_header: block::Height,
    ) -> BoxFuture<'static, Result<Vec<BlockSyncBlockMeta>, zebra_chain::BoxError>> {
        self.inner.query_missing_bodies(verified_body, best_header)
    }

    /// Submit a downloaded body to the verifier/state pipeline.
    pub fn submit_block_body(
        &self,
        token: BlockApplyToken,
        block: Arc<block::Block>,
    ) -> BoxFuture<'static, BlockBodySubmit> {
        self.inner.submit_block_body(token, block)
    }

    /// Read committed blocks for serving stream-6 peers.
    pub fn read_committed_blocks(
        &self,
        start: block::Height,
        count: u32,
    ) -> BoxFuture<
        'static,
        Result<Vec<(block::Height, Arc<block::Block>, usize)>, zebra_chain::BoxError>,
    > {
        self.inner.read_committed_blocks(start, count)
    }

    /// Publish body-sync progress that did not come from a direct submit path.
    pub fn publish_body_progress(&self, update: FrontierUpdate) -> BoxFuture<'static, ()> {
        self.inner.publish_body_progress(update)
    }
}

impl std::fmt::Debug for BlockSyncStatePort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BlockSyncStatePort")
    }
}

/// Build a [`ChainFrontier`] from startup pieces that do not expose finalized hashes.
pub fn chain_frontier_from_parts(
    finalized_height: block::Height,
    verified_body: Frontier,
    best_header: Frontier,
) -> ChainFrontier {
    let finalized_hash = if finalized_height == verified_body.height {
        verified_body.hash
    } else {
        block::Hash([0; 32])
    };

    ChainFrontier {
        finalized: Frontier::new(finalized_height, finalized_hash),
        verified_body,
        best_header,
    }
}

/// Apply one requested frontier update to the current exchange frontier.
///
/// Returns `None` when the requested update is stale or redundant.
pub fn apply_frontier_update(
    current: FrontierUpdate,
    requested: FrontierUpdate,
) -> Option<FrontierUpdate> {
    let mut frontier = current.frontier;

    match requested.change {
        FrontierChange::Snapshot => {
            frontier.finalized =
                higher_frontier(current.frontier.finalized, requested.frontier.finalized);
            frontier.verified_body = higher_frontier(
                current.frontier.verified_body,
                requested.frontier.verified_body,
            );
            frontier.best_header =
                higher_frontier(current.frontier.best_header, requested.frontier.best_header);
            if frontier == current.frontier {
                return None;
            }
        }
        FrontierChange::VerifiedGrow => {
            if requested.frontier.verified_body.height < current.frontier.verified_body.height {
                return None;
            }
            frontier.finalized =
                higher_frontier(current.frontier.finalized, requested.frontier.finalized);
            frontier.verified_body = requested.frontier.verified_body;
        }
        FrontierChange::VerifiedReset => {
            frontier.finalized =
                higher_frontier(current.frontier.finalized, requested.frontier.finalized);
            frontier.verified_body = requested.frontier.verified_body;
        }
        FrontierChange::HeaderAdvanced => {
            if requested.frontier.best_header.height <= current.frontier.best_header.height {
                return None;
            }
            frontier.best_header = requested.frontier.best_header;
        }
        FrontierChange::HeaderReanchored => {
            if requested.frontier.best_header == current.frontier.best_header {
                return None;
            }
            frontier.best_header = requested.frontier.best_header;
        }
    }

    Some(FrontierUpdate {
        frontier,
        change: requested.change,
    })
}

fn higher_frontier(left: Frontier, right: Frontier) -> Frontier {
    if right.height > left.height {
        right
    } else {
        left
    }
}

fn frontier_change_label(change: FrontierChange) -> &'static str {
    match change {
        FrontierChange::Snapshot => "snapshot",
        FrontierChange::VerifiedGrow => "verified_grow",
        FrontierChange::VerifiedReset => "verified_reset",
        FrontierChange::HeaderAdvanced => "header_advanced",
        FrontierChange::HeaderReanchored => "header_reanchored",
    }
}

fn insert_frontier_fields(
    row: &mut serde_json::Map<String, Value>,
    prefix: &'static str,
    frontier: ChainFrontier,
) {
    let (
        finalized_height,
        finalized_hash,
        verified_body_height,
        verified_body_hash,
        best_header_height,
        best_header_hash,
    ) = match prefix {
        "old" => (
            cs_trace::OLD_FINALIZED_HEIGHT,
            cs_trace::OLD_FINALIZED_HASH,
            cs_trace::OLD_VERIFIED_BODY_HEIGHT,
            cs_trace::OLD_VERIFIED_BODY_HASH,
            cs_trace::OLD_BEST_HEADER_HEIGHT,
            cs_trace::OLD_BEST_HEADER_HASH,
        ),
        "new" => (
            cs_trace::NEW_FINALIZED_HEIGHT,
            cs_trace::NEW_FINALIZED_HASH,
            cs_trace::NEW_VERIFIED_BODY_HEIGHT,
            cs_trace::NEW_VERIFIED_BODY_HASH,
            cs_trace::NEW_BEST_HEADER_HEIGHT,
            cs_trace::NEW_BEST_HEADER_HASH,
        ),
        _ => return,
    };

    insert_height(row, finalized_height, frontier.finalized.height);
    insert_hash(row, finalized_hash, frontier.finalized.hash);
    insert_height(row, verified_body_height, frontier.verified_body.height);
    insert_hash(row, verified_body_hash, frontier.verified_body.hash);
    insert_height(row, best_header_height, frontier.best_header.height);
    insert_hash(row, best_header_hash, frontier.best_header.hash);
}

fn insert_height(
    row: &mut serde_json::Map<String, Value>,
    key: &'static str,
    height: block::Height,
) {
    row.insert(
        key.to_string(),
        Value::Number(Number::from(u64::from(height.0))),
    );
}

fn insert_hash(row: &mut serde_json::Map<String, Value>, key: &'static str, hash: block::Hash) {
    row.insert(key.to_string(), Value::String(format!("{hash}")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Barrier},
        thread,
    };

    fn frontier(height: u32, seed: u8) -> Frontier {
        Frontier::new(block::Height(height), block::Hash([seed; 32]))
    }

    fn update(
        finalized: Frontier,
        verified_body: Frontier,
        best_header: Frontier,
        change: FrontierChange,
    ) -> FrontierUpdate {
        FrontierUpdate {
            frontier: ChainFrontier {
                finalized,
                verified_body,
                best_header,
            },
            change,
        }
    }

    #[test]
    fn grow_advances_verified_body() {
        let current = update(
            frontier(1, 1),
            frontier(2, 2),
            frontier(5, 5),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(3, 3),
            frontier(4, 4),
            frontier(9, 9),
            FrontierChange::VerifiedGrow,
        );

        let updated = apply_frontier_update(current, requested).expect("grow is accepted");

        assert_eq!(updated.frontier.verified_body, frontier(4, 4));
        assert_eq!(updated.frontier.best_header, frontier(5, 5));
    }

    #[test]
    fn grow_may_not_lower_best_header() {
        let current = update(
            frontier(1, 1),
            frontier(2, 2),
            frontier(9, 9),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(3, 3),
            frontier(4, 4),
            frontier(5, 5),
            FrontierChange::VerifiedGrow,
        );

        let updated = apply_frontier_update(current, requested).expect("grow is accepted");

        assert_eq!(updated.frontier.finalized, frontier(3, 3));
        assert_eq!(updated.frontier.verified_body, frontier(4, 4));
        assert_eq!(updated.frontier.best_header, frontier(9, 9));
    }

    #[test]
    fn stale_lower_grow_is_ignored() {
        let current = update(
            frontier(1, 1),
            frontier(8, 8),
            frontier(9, 9),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(1, 1),
            frontier(7, 7),
            frontier(9, 9),
            FrontierChange::VerifiedGrow,
        );

        assert!(apply_frontier_update(current, requested).is_none());
    }

    #[test]
    fn equal_height_grow_refreshes_verified_hash() {
        let current = update(
            frontier(1, 1),
            frontier(8, 8),
            frontier(9, 9),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(1, 1),
            frontier(8, 18),
            frontier(9, 19),
            FrontierChange::VerifiedGrow,
        );

        let updated =
            apply_frontier_update(current, requested).expect("equal height grow is accepted");

        assert_eq!(updated.frontier.verified_body, frontier(8, 18));
        assert_eq!(updated.frontier.best_header, frontier(9, 9));
    }

    #[test]
    fn reset_may_lower_verified_body() {
        let current = update(
            frontier(5, 5),
            frontier(8, 8),
            frontier(10, 10),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(4, 4),
            frontier(6, 6),
            frontier(2, 2),
            FrontierChange::VerifiedReset,
        );

        let updated = apply_frontier_update(current, requested).expect("reset is accepted");

        assert_eq!(updated.frontier.finalized, frontier(5, 5));
        assert_eq!(updated.frontier.verified_body, frontier(6, 6));
        assert_eq!(updated.frontier.best_header, frontier(10, 10));
    }

    #[test]
    fn finalized_height_never_decreases_across_mixed_updates() {
        let mut current = update(
            frontier(10, 10),
            frontier(10, 10),
            frontier(12, 12),
            FrontierChange::Snapshot,
        );
        for requested in [
            update(
                frontier(9, 9),
                frontier(11, 11),
                frontier(12, 12),
                FrontierChange::VerifiedGrow,
            ),
            update(
                frontier(1, 1),
                frontier(7, 7),
                frontier(0, 0),
                FrontierChange::VerifiedReset,
            ),
            update(
                frontier(8, 8),
                frontier(8, 8),
                frontier(20, 20),
                FrontierChange::HeaderAdvanced,
            ),
            update(
                frontier(3, 3),
                frontier(3, 3),
                frontier(13, 13),
                FrontierChange::HeaderReanchored,
            ),
        ] {
            if let Some(updated) = apply_frontier_update(current, requested) {
                current = updated;
                assert!(
                    current.frontier.finalized.height >= block::Height(10),
                    "finalized frontier must never move below the original finalized height"
                );
            }
        }
    }

    #[test]
    fn snapshot_does_not_lower_existing_frontiers() {
        let current = update(
            frontier(5, 5),
            frontier(8, 8),
            frontier(12, 12),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(4, 4),
            frontier(7, 7),
            frontier(11, 11),
            FrontierChange::Snapshot,
        );

        assert!(apply_frontier_update(current, requested).is_none());
    }

    #[test]
    fn header_reanchor_lowers_only_best_header() {
        let current = update(
            frontier(5, 5),
            frontier(8, 8),
            frontier(12, 12),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(1, 1),
            frontier(2, 2),
            frontier(9, 9),
            FrontierChange::HeaderReanchored,
        );

        let updated = apply_frontier_update(current, requested).expect("reanchor is accepted");

        assert_eq!(updated.frontier.finalized, frontier(5, 5));
        assert_eq!(updated.frontier.verified_body, frontier(8, 8));
        assert_eq!(updated.frontier.best_header, frontier(9, 9));
    }

    #[test]
    fn header_advance_cannot_change_verified_body() {
        let current = update(
            frontier(5, 5),
            frontier(8, 8),
            frontier(12, 12),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(1, 1),
            frontier(2, 2),
            frontier(13, 13),
            FrontierChange::HeaderAdvanced,
        );

        let updated = apply_frontier_update(current, requested).expect("advance is accepted");

        assert_eq!(updated.frontier.finalized, frontier(5, 5));
        assert_eq!(updated.frontier.verified_body, frontier(8, 8));
        assert_eq!(updated.frontier.best_header, frontier(13, 13));
    }

    #[test]
    fn stale_header_advance_is_ignored() {
        let current = update(
            frontier(5, 5),
            frontier(8, 8),
            frontier(12, 12),
            FrontierChange::Snapshot,
        );
        let requested = update(
            frontier(20, 20),
            frontier(21, 21),
            frontier(12, 22),
            FrontierChange::HeaderAdvanced,
        );

        assert!(apply_frontier_update(current, requested).is_none());
    }

    #[test]
    fn exchange_keeps_latest_value_without_external_receivers() {
        let initial = update(
            frontier(0, 0),
            frontier(0, 0),
            frontier(0, 0),
            FrontierChange::Snapshot,
        );
        let exchange = ZakuraSyncExchange::new(initial, ZakuraTrace::noop());
        let accepted = update(
            frontier(0, 0),
            frontier(0, 0),
            frontier(10, 10),
            FrontierChange::HeaderAdvanced,
        );

        exchange.publish_frontier(accepted, "test");

        assert_eq!(exchange.current_frontier(), accepted);

        let stale = update(
            frontier(0, 0),
            frontier(0, 0),
            frontier(9, 9),
            FrontierChange::HeaderAdvanced,
        );
        exchange.publish_frontier(stale, "test");

        assert_eq!(exchange.current_frontier(), accepted);
    }

    #[test]
    fn concurrent_publishers_converge_to_highest_frontiers() {
        const PUBLISHERS: u32 = 64;

        let initial = update(
            frontier(0, 0),
            frontier(0, 0),
            frontier(0, 0),
            FrontierChange::Snapshot,
        );
        let exchange = ZakuraSyncExchange::new(initial, ZakuraTrace::noop());
        let barrier = Arc::new(Barrier::new((PUBLISHERS * 2 + 1) as usize));
        let mut publishers = Vec::new();

        for height in 1..=PUBLISHERS {
            let seed = u8::try_from(height).expect("test publisher height fits in u8");
            let header_exchange = exchange.clone();
            let header_barrier = barrier.clone();
            publishers.push(thread::spawn(move || {
                header_barrier.wait();
                header_exchange.publish_frontier(
                    update(
                        frontier(0, 0),
                        frontier(0, 0),
                        frontier(height, seed),
                        FrontierChange::HeaderAdvanced,
                    ),
                    "test",
                );
            }));

            let seed = u8::try_from(height).expect("test publisher height fits in u8");
            let grow_exchange = exchange.clone();
            let grow_barrier = barrier.clone();
            publishers.push(thread::spawn(move || {
                grow_barrier.wait();
                grow_exchange.publish_frontier(
                    update(
                        frontier(height, seed),
                        frontier(height, seed),
                        frontier(0, 0),
                        FrontierChange::VerifiedGrow,
                    ),
                    "test",
                );
            }));
        }

        barrier.wait();
        for publisher in publishers {
            publisher.join().expect("publisher thread should not panic");
        }

        let current = exchange.current_frontier().frontier;
        let seed = u8::try_from(PUBLISHERS).expect("test publisher height fits in u8");
        assert_eq!(current.finalized, frontier(PUBLISHERS, seed));
        assert_eq!(current.verified_body, frontier(PUBLISHERS, seed));
        assert_eq!(current.best_header, frontier(PUBLISHERS, seed));
    }

    #[test]
    fn concurrent_header_advance_is_not_lost_to_verified_grow() {
        let initial = update(
            frontier(0, 0),
            frontier(5, 5),
            frontier(10, 10),
            FrontierChange::Snapshot,
        );
        let exchange = ZakuraSyncExchange::new(initial, ZakuraTrace::noop());
        let barrier = Arc::new(Barrier::new(3));

        let header_exchange = exchange.clone();
        let header_barrier = barrier.clone();
        let header = thread::spawn(move || {
            header_barrier.wait();
            header_exchange.publish_frontier(
                update(
                    frontier(0, 0),
                    frontier(0, 0),
                    frontier(20, 20),
                    FrontierChange::HeaderAdvanced,
                ),
                "test",
            );
        });

        let grow_exchange = exchange.clone();
        let grow_barrier = barrier.clone();
        let grow = thread::spawn(move || {
            grow_barrier.wait();
            grow_exchange.publish_frontier(
                update(
                    frontier(0, 0),
                    frontier(8, 8),
                    frontier(0, 0),
                    FrontierChange::VerifiedGrow,
                ),
                "test",
            );
        });

        barrier.wait();
        header.join().expect("header publisher should not panic");
        grow.join().expect("grow publisher should not panic");

        let current = exchange.current_frontier().frontier;
        assert_eq!(current.verified_body, frontier(8, 8));
        assert_eq!(current.best_header, frontier(20, 20));
    }
}
