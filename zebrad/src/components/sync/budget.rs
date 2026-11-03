//! The sync pipeline's memory limits, derived from the memory budget.
//!
//! With a byte bound, every block the syncer or the inbound block downloader holds is charged
//! until its accepted owner commits, rejects, or evicts it (design §4.2).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    mem::size_of,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use tokio::{
    sync::{mpsc, watch},
    time::{sleep_until, Instant},
};

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::ChainTip,
    parameters::checkpoint::list::CheckpointList,
    transaction,
};
use zebra_state::BlockRetentionToken;

#[cfg(test)]
mod tests;

/// How many times its serialized size one checkpoint range of blocks can charge while it waits
/// in the sync pipeline, decoded and with its metadata.
///
/// The oldest checkpoint range is always admitted, so the `Ibd` pipeline floor is
/// [`MAX_CHECKPOINT_BYTE_COUNT`](zebra_chain::parameters::checkpoint::constants::MAX_CHECKPOINT_BYTE_COUNT)
/// times this ratio (design §2.3, §4.2 step 3).
pub const MAX_CHECKPOINT_CHARGE_RATIO: u64 = 5;

/// Memory limits for the block sync pipeline, derived from zebrad's memory budget.
///
/// [`SyncMemoryLimits::default()`] is today's behaviour: the pipeline is bounded only by its
/// block counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncMemoryLimits {
    /// The retained bytes of downloaded blocks that are not yet committed, above which the syncer
    /// waits before downloading more blocks.
    ///
    /// `None` applies no byte bound.
    pub pipeline_bytes: Option<u64>,
}

/// A handle to the sync pipeline's byte bound, shared by the syncer and the inbound block
/// downloader.
///
/// The default handle has no bound: it never charges, never waits and never drops a block, so a
/// node without a memory budget keeps today's behaviour.
#[derive(Clone, Debug, Default)]
pub struct PipelineBudget {
    /// The shared charge and its limit, or `None` without a byte bound.
    bound: Option<Arc<Bound>>,
}

/// The shared state of a bounded [`PipelineBudget`].
#[derive(Debug)]
struct Bound {
    /// The retained bytes of every downloaded block that is still in the pipeline.
    charged: AtomicU64,

    /// The current `pipeline_bytes`, updated when the memory plan changes profile.
    limit: AtomicU64,

    /// Notified after every release or limit update, so waiting downloads re-check admission.
    released: watch::Sender<()>,
}

impl PipelineBudget {
    /// Returns a handle bounded by `limits.pipeline_bytes`, or an unbounded handle without one.
    pub fn new(limits: SyncMemoryLimits) -> Self {
        let bound = limits.pipeline_bytes.map(|limit| {
            Arc::new(Bound {
                charged: AtomicU64::new(0),
                limit: AtomicU64::new(limit),
                released: watch::Sender::new(()),
            })
        });

        Self { bound }
    }

    /// Returns `true` if this pipeline has a byte bound.
    pub fn is_bounded(&self) -> bool {
        self.bound.is_some()
    }

    /// Returns the current byte bound, or `None` without one.
    pub fn limit(&self) -> Option<u64> {
        self.bound.as_ref().map(|bound| bound.limit())
    }

    /// Changes the admission ceiling, updates its metrics, and wakes waiting sync admissions.
    ///
    /// Existing charges are unchanged. Unbounded handles stay unbounded and ignore updates.
    pub fn update_limit(&self, limit: u64) {
        let Some(bound) = self.bound.as_ref() else {
            return;
        };

        bound.limit.store(limit, Ordering::Release);
        bound.export_metrics(bound.charged.load(Ordering::Acquire));
        bound.released.send_replace(());
    }

    /// Returns the retained bytes currently charged to the pipeline, 0 without a byte bound.
    pub fn charged_bytes(&self) -> u64 {
        self.bound
            .as_ref()
            .map_or(0, |bound| bound.charged.load(Ordering::Acquire))
    }

    /// Returns `true` if the charge is above the current byte bound.
    pub fn is_over_limit(&self) -> bool {
        self.bound
            .as_ref()
            .is_some_and(|bound| bound.charged.load(Ordering::Acquire) > bound.limit())
    }

    /// Charges `bytes` to the pipeline until the returned guard is dropped, unless that takes
    /// the charge above the bound (design §4.2 step 5).
    ///
    /// Returns `Ok(None)` without a byte bound, and [`OverLimit`] without charging if the bytes
    /// don't fit. Never waits.
    pub(crate) fn try_charge(&self, bytes: u64) -> Result<Option<PipelineCharge>, OverLimit> {
        let Some(bound) = self.bound.as_ref() else {
            return Ok(None);
        };

        let charged = bound
            .charged
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |charged| {
                charged
                    .checked_add(bytes)
                    .filter(|charged| *charged <= bound.limit())
            })
            .map_err(|charged| OverLimit {
                charged,
                limit: bound.limit(),
            })?
            .saturating_add(bytes);
        bound.export_metrics(charged);

        Ok(Some(PipelineCharge {
            bound: bound.clone(),
            bytes,
        }))
    }

    /// Charges `bytes` to the pipeline until the returned guard is dropped.
    ///
    /// Returns `None` without a byte bound.
    pub(crate) fn charge(&self, bytes: u64) -> Option<PipelineCharge> {
        Some(self.bound.as_ref()?.charge(bytes))
    }
}

impl Bound {
    /// Returns the current admission ceiling.
    fn limit(&self) -> u64 {
        self.limit.load(Ordering::Acquire)
    }

    /// Charges `bytes` even above this bound's limit, as required for sync checkpoint liveness.
    fn charge(self: &Arc<Self>, bytes: u64) -> PipelineCharge {
        let charged = self
            .charged
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |charged| {
                charged.checked_add(bytes)
            })
            .expect("live downloaded blocks own their memory, so their total charge fits in u64")
            .checked_add(bytes)
            .expect("the successful atomic update already checked this addition");
        self.export_metrics(charged);

        PipelineCharge {
            bound: self.clone(),
            bytes,
        }
    }

    /// Exports the `charged` gauge and its excess over the limit (design §5).
    fn export_metrics(&self, charged: u64) {
        // Gauges are `f64`; byte counts lose precision only above 2^53 bytes.
        metrics::gauge!("sync.pipeline.charged_bytes").set(charged as f64);
        metrics::gauge!("sync.pipeline.over_admitted_bytes")
            .set(charged.saturating_sub(self.limit()) as f64);
    }
}

/// A block's charge did not fit under the pipeline's byte bound.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct OverLimit {
    /// The charge when the block was refused.
    pub charged: u64,

    /// The byte bound.
    pub limit: u64,
}

/// The charge of one downloaded block, released when it is dropped.
///
/// A [`BlockRetentionToken`] can jointly own this guard with accepted verifier and state owners,
/// keeping it alive after the caller's download task ends.
#[derive(Debug)]
#[must_use = "the charge is released when the guard is dropped"]
pub(crate) struct PipelineCharge {
    /// The pipeline the bytes are charged to.
    bound: Arc<Bound>,

    /// The block's charge, computed once after its download.
    bytes: u64,
}

impl Drop for PipelineCharge {
    fn drop(&mut self) {
        let charged = self
            .bound
            .charged
            .fetch_sub(self.bytes, Ordering::AcqRel)
            .checked_sub(self.bytes)
            .expect(
                "each guard releases exactly the bytes it added, so the charge never underflows",
            );
        self.bound.export_metrics(charged);
        self.bound.released.send_replace(());
    }
}

/// Computes [`downloaded_block_charge`] on a blocking thread.
///
/// Returns `None` if the runtime is shutting down, so nothing will verify the block.
pub(crate) async fn compute_downloaded_block_charge(block: Arc<Block>) -> Option<u64> {
    match tokio::task::spawn_blocking(move || downloaded_block_charge(&block)).await {
        Ok(bytes) => Some(bytes),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_cancelled) => None,
    }
}

/// Returns the retained charge of a downloaded block: its serialized size plus its decoded
/// structures and its share of the state's metadata (design §4.2 step 1).
///
/// Equals [`zebra_state::block_retained_charge`] with the block's transaction hashes, which only
/// counts the hashes by their size, so it doesn't hash the transactions.
///
/// This walks the whole block, so call it once per block on a blocking thread.
pub(crate) fn downloaded_block_charge(block: &Block) -> u64 {
    let charge = zebra_state::block_retained_charge(block, &[]).saturating_add(
        block
            .transactions
            .len()
            .saturating_mul(size_of::<transaction::Hash>()),
    );

    u64::try_from(charge).expect("usize fits in u64 on supported platforms")
}

/// What a sync download task reports to its downloader's [`PipelineAdmission`].
#[derive(Debug)]
pub(super) enum PipelineEvent {
    /// The block was downloaded at `height` and charged `bytes`.
    Downloaded {
        hash: block::Hash,
        height: Height,
        bytes: u64,
    },

    /// The task ended, after releasing its charge.
    Finished { hash: block::Hash },
}

/// The pipeline stayed above its byte bound, so a download was not started.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Deferred;

/// The syncer's admission state for a byte-bounded pipeline, owned by its `Downloads`
/// (design §4.2 steps 1 and 2).
///
/// Download tasks report through an unbounded channel that this state drains before each
/// admission decision, so the per-download bookkeeping needs no lock. The channel holds at most
/// two reports per queued download, and the syncer's lookahead limit bounds the queued downloads.
#[derive(Debug)]
pub(super) struct PipelineAdmission {
    /// The pipeline's byte bound, shared with the inbound block downloader.
    budget: PipelineBudget,

    /// The checkpoints the checkpoint verifier uses.
    checkpoints: Arc<CheckpointList>,

    /// The highest checkpoint the checkpoint verifier uses: above it, every block commits on its
    /// own.
    max_checkpoint_height: Height,

    /// Every queued download: `None` until it reports its height and charge.
    downloaded: HashMap<block::Hash, Option<(Height, u64)>>,

    /// How many entries of `downloaded` have each height.
    heights: BTreeMap<Height, usize>,

    /// How many entries of `downloaded` have no height yet.
    unknown_heights: usize,

    /// Downloads whose task finished, but whose result the syncer has not observed yet.
    finished: HashSet<block::Hash>,

    /// Cloned into every download task.
    events_tx: mpsc::UnboundedSender<PipelineEvent>,

    /// Reports from the download tasks.
    events_rx: mpsc::UnboundedReceiver<PipelineEvent>,

    /// Changes after every release of a charge, including the inbound downloader's.
    releases: watch::Receiver<()>,

    /// When the last download was admitted, for the stall gauge.
    last_admitted: Instant,
}

impl PipelineAdmission {
    /// Returns the admission state of a bounded `budget`, or `None` if it has no byte bound.
    pub(super) fn new(
        budget: PipelineBudget,
        checkpoints: Arc<CheckpointList>,
        max_checkpoint_height: Height,
    ) -> Option<Self> {
        let releases = budget.bound.as_ref()?.released.subscribe();
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        Some(Self {
            budget,
            checkpoints,
            max_checkpoint_height,
            downloaded: HashMap::new(),
            heights: BTreeMap::new(),
            unknown_heights: 0,
            finished: HashSet::new(),
            events_tx,
            events_rx,
            releases,
            last_admitted: Instant::now(),
        })
    }

    /// Returns the pipeline's byte bound.
    pub(super) fn budget(&self) -> &PipelineBudget {
        &self.budget
    }

    /// Waits until a new download may start, or returns [`Deferred`] (design §4.2 step 2).
    ///
    /// A download waits while the charge is above the bound and the oldest checkpoint range is
    /// provably fully requested; otherwise it is admitted at once, even above the bound. A waiting
    /// download is admitted as soon as either condition stops holding. It is deferred at the
    /// `deadline`, or as soon as a download task finishes whose result the syncer hasn't
    /// observed, so the syncer handles that result (re-obtaining a missing block, for example)
    /// before it asks again.
    pub(super) async fn admit(
        &mut self,
        latest_chain_tip: &impl ChainTip,
        deadline: Instant,
    ) -> Result<(), Deferred> {
        let wait_start = Instant::now();
        let mut waited = false;

        loop {
            self.drain();

            if !self.must_wait(latest_chain_tip.best_tip_height()) {
                if waited {
                    metrics::histogram!("sync.pipeline.admission_wait_seconds")
                        .record(wait_start.elapsed().as_secs_f64());
                }
                self.admitted();
                return Ok(());
            }

            metrics::gauge!("sync.pipeline.blocked_seconds")
                .set(self.last_admitted.elapsed().as_secs_f64());

            if !self.finished.is_empty() {
                // Let the finished task complete, so the syncer can observe its result.
                tokio::task::yield_now().await;
                return Err(Deferred);
            }

            waited = true;

            tokio::select! {
                event = self.events_rx.recv() => {
                    let event = event
                        .expect("the admission state holds a sender, so its channel never closes");
                    self.apply(event);
                }
                // The sender lives in the shared budget this state holds, so this never fails.
                _ = self.releases.changed() => {}
                _ = sleep_until(deadline) => return Err(Deferred),
            }
        }
    }

    /// Records an admitted download, including one admitted without waiting.
    pub(super) fn admitted(&mut self) {
        self.last_admitted = Instant::now();
        metrics::gauge!("sync.pipeline.blocked_seconds").set(0.0);
    }

    /// Starts tracking a newly queued download of `hash`, returning the tracker its task holds.
    pub(super) fn track(&mut self, hash: block::Hash) -> TrackedDownload {
        // The reports of an earlier download of this hash precede its result, so they were
        // applied when the syncer observed that result, or are applied here.
        self.drain();
        self.forget(&hash);

        self.downloaded.insert(hash, None);
        self.unknown_heights = self.unknown_heights.saturating_add(1);

        TrackedDownload {
            hash,
            budget: self.budget.clone(),
            events: self.events_tx.clone(),
            retention: None,
        }
    }

    /// Forgets the download of `hash`, after the syncer observed its result.
    pub(super) fn observe(&mut self, hash: &block::Hash) {
        // The task reported `Finished` before its result was available, so this applies every
        // report of this download.
        self.drain();
        self.forget(hash);
        self.finished.remove(hash);
    }

    /// Forgets every download, and ignores the reports of their cancelled tasks.
    pub(super) fn clear(&mut self) {
        self.downloaded.clear();
        self.heights.clear();
        self.unknown_heights = 0;
        self.finished.clear();

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        self.events_tx = events_tx;
        self.events_rx = events_rx;
    }

    /// Returns `true` if no download is tracked.
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.downloaded.is_empty()
            && self.heights.is_empty()
            && self.unknown_heights == 0
            && self.finished.is_empty()
    }

    /// Returns `true` if a new download must wait for the charge to drop.
    fn must_wait(&self, tip: Option<Height>) -> bool {
        self.budget.is_over_limit()
            && oldest_range_is_fully_requested(
                &self.checkpoints,
                self.max_checkpoint_height,
                tip,
                &self.heights,
                self.unknown_heights,
            )
    }

    /// Applies every report the download tasks have sent.
    fn drain(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            self.apply(event);
        }
    }

    /// Applies one report from a download task.
    fn apply(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::Downloaded {
                hash,
                height,
                bytes,
            } => {
                if let Some(entry @ None) = self.downloaded.get_mut(&hash) {
                    *entry = Some((height, bytes));
                    self.unknown_heights = self
                        .unknown_heights
                        .checked_sub(1)
                        .expect("every tracked download without a height is counted");
                    *self.heights.entry(height).or_default() += 1;
                }
            }
            PipelineEvent::Finished { hash } => {
                if self.forget(&hash) {
                    self.finished.insert(hash);
                }
            }
        }
    }

    /// Stops counting the download of `hash`, returning `true` if it was tracked.
    fn forget(&mut self, hash: &block::Hash) -> bool {
        match self.downloaded.remove(hash) {
            None => false,
            Some(None) => {
                self.unknown_heights = self
                    .unknown_heights
                    .checked_sub(1)
                    .expect("every tracked download without a height is counted");
                true
            }
            Some(Some((height, _bytes))) => {
                let count = self
                    .heights
                    .get_mut(&height)
                    .expect("every tracked download with a height is counted");
                *count -= 1;
                if *count == 0 {
                    self.heights.remove(&height);
                }
                true
            }
        }
    }
}

/// Returns `true` if the oldest checkpoint range above `tip` is provably fully requested, or if
/// there is no checkpoint range to protect (design §4.2 step 2, the structural liveness rule).
///
/// The range is the heights from `tip + 1` to the next checkpoint the verifier uses. It is
/// provably fully requested when its distinct downloaded heights plus the downloads with unknown
/// heights cover all of it: counting unknown heights as in the range can only delay a wait, never
/// a request. Hashes are requested in height order, so while the range is not fully requested,
/// the next hash is either in it or the range is complete.
///
/// Above the last checkpoint the verifier uses every block commits on its own, and an empty
/// state requests the genesis block without waiting, so neither has a range to protect.
fn oldest_range_is_fully_requested(
    checkpoints: &CheckpointList,
    max_checkpoint_height: Height,
    tip: Option<Height>,
    downloaded_heights: &BTreeMap<Height, usize>,
    unknown_heights: usize,
) -> bool {
    let Some(tip) = tip else {
        return true;
    };
    let Some(first) = (tip + 1).filter(|first| *first <= max_checkpoint_height) else {
        return true;
    };
    let Some(next_checkpoint) = checkpoints.min_height_in_range(first..=max_checkpoint_height)
    else {
        return true;
    };

    let range_len = u64::from(next_checkpoint.0 - tip.0);
    let requested = downloaded_heights
        .range(first..=next_checkpoint)
        .count()
        .saturating_add(unknown_heights);

    u64::try_from(requested).unwrap_or(u64::MAX) >= range_len
}

/// A queued download's link to its downloader's [`PipelineAdmission`], held by its task.
///
/// Dropping it releases the task's token, then reports that the task finished. Accepted downstream
/// owners can keep other clones of the block's charge alive after either event.
#[derive(Debug)]
pub(super) struct TrackedDownload {
    /// The downloaded block's hash.
    hash: block::Hash,

    /// The pipeline's byte bound.
    budget: PipelineBudget,

    /// Reports to the downloader.
    events: mpsc::UnboundedSender<PipelineEvent>,

    /// The block's charge, once it is downloaded.
    retention: Option<BlockRetentionToken>,
}

impl TrackedDownload {
    /// Charges the block downloaded at `height` to the pipeline and reports it.
    pub(super) fn downloaded(&mut self, height: Height, bytes: u64) {
        self.retention = self.budget.charge(bytes).map(|charge| {
            BlockRetentionToken::new(charge).with_retained_charge(
                usize::try_from(bytes).expect("the block charge was converted from usize"),
            )
        });

        // A reset or dropped downloader no longer counts this download.
        let _ = self.events.send(PipelineEvent::Downloaded {
            hash: self.hash,
            height,
            bytes,
        });
    }

    /// Returns a clone jointly owned with the block's verifier and state commit owners.
    pub(super) fn retention(&self) -> Option<BlockRetentionToken> {
        self.retention.clone()
    }
}

impl Drop for TrackedDownload {
    fn drop(&mut self) {
        // Release this task's token before reporting, without dropping any downstream clones.
        drop(self.retention.take());

        // A reset or dropped downloader no longer counts this download.
        let _ = self
            .events
            .send(PipelineEvent::Finished { hash: self.hash });
    }
}
