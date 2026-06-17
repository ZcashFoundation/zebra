use super::{state::*, *};

pub(super) const DEFAULT_BS_EWMA_SEED_BYTES: u64 = 256 * 1024;
pub(super) const DEFAULT_BS_SIZE_FLOOR_BYTES: u64 = 1024;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NeededBlock {
    pub(super) height: block::Height,
    pub(super) hash: block::Hash,
    pub(super) size: BlockSizeEstimate,
}

/// Scheduling source for a block-body byte estimate.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BlockSizeEstimate {
    /// Confirmed serialized size from committed block metadata.
    Confirmed(u32),
    /// Untrusted advertised size hint from header sync.
    Advertised(u32),
    /// No size hint is known; use the EWMA fallback.
    Unknown,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum ScheduleSkipReason {
    NoAssignableRange,
    PeerByteCapExhausted,
    BudgetExhausted,
    FirstBlockExceedsByteLimit,
    ReserveFailed,
    RequestCountOverflow,
}

impl ScheduleSkipReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ScheduleSkipReason::NoAssignableRange => "no_assignable_range",
            ScheduleSkipReason::PeerByteCapExhausted => "peer_byte_cap_exhausted",
            ScheduleSkipReason::BudgetExhausted => "budget_exhausted",
            ScheduleSkipReason::FirstBlockExceedsByteLimit => "first_block_exceeds_byte_limit",
            ScheduleSkipReason::ReserveFailed => "reserve_failed",
            ScheduleSkipReason::RequestCountOverflow => "request_count_overflow",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct BlockRangeScheduler {
    queue: VecDeque<BlockRange>,
    assigned: HashMap<BlockRangeKey, HashSet<ZakuraPeerId>>,
    covered: Vec<CoveredRange>,
    fanout: usize,
    ewma_fallback_bytes: u64,
    floor_estimate_bytes: u64,
}

impl BlockRangeScheduler {
    pub(super) fn new(fanout: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            assigned: HashMap::new(),
            covered: Vec::new(),
            fanout: fanout.max(1),
            ewma_fallback_bytes: DEFAULT_BS_EWMA_SEED_BYTES,
            floor_estimate_bytes: DEFAULT_BS_SIZE_FLOOR_BYTES,
        }
    }

    #[cfg(test)]
    pub(super) fn set_estimator_for_tests(&mut self, ewma: u64, floor: u64) {
        self.ewma_fallback_bytes = ewma.max(1);
        self.floor_estimate_bytes = floor.max(1);
    }

    pub(super) fn refresh_needed(&mut self, mut needed: Vec<NeededBlock>) {
        needed.sort_by_key(|block| block.height);
        needed.dedup_by_key(|block| block.height);

        let mut range: Option<BlockRange> = None;
        for block in needed {
            let estimate = self.estimate_bytes(block.size);
            if let Some(current) = range.as_mut() {
                if current.end_height().0.checked_add(1) == Some(block.height.0) {
                    current.blocks.push(ScheduledBlock {
                        height: block.height,
                        hash: block.hash,
                        estimated_bytes: estimate,
                    });
                    continue;
                }
            }
            if let Some(current) = range.take() {
                self.ensure(current);
            }
            range = Some(BlockRange {
                blocks: vec![ScheduledBlock {
                    height: block.height,
                    hash: block.hash,
                    estimated_bytes: estimate,
                }],
            });
        }
        if let Some(current) = range {
            self.ensure(current);
        }
        self.prune_covered();
    }

    pub(super) fn retain_matching_needed(&mut self, needed: &HashMap<block::Height, block::Hash>) {
        self.queue.retain(|range| range.matches_needed(needed));
        // Active assignments are paired with outstanding requests, which the
        // reactor hash-prunes before retaining scheduler state.
        self.assigned.retain(|range, _| {
            needed
                .get(&range.start)
                .is_some_and(|_| needed.contains_key(&range.end))
        });
    }

    pub(super) fn clear_queued(&mut self) {
        self.queue.clear();
    }

    pub(super) fn drop_through(&mut self, tip: block::Height) {
        for range in &mut self.queue {
            range.blocks.retain(|block| block.height > tip);
        }
        self.queue.retain(|range| !range.blocks.is_empty());
        self.assigned.retain(|range, _| range.end > tip);
    }

    pub(super) fn next_for_peer(
        &mut self,
        peer_id: &ZakuraPeerId,
        peer: &PeerBlockState,
        budget: &mut ByteBudget,
        per_peer_byte_cap: u64,
    ) -> Result<BlockRangeRequest, ScheduleSkipReason> {
        self.prune_covered();
        let index = self
            .queue
            .iter()
            .position(|range| {
                range.end_height() <= peer.servable_high
                    && range.start_height() >= peer.servable_low
                    && self.can_assign_peer_to_range(peer_id, range)
            })
            .ok_or(ScheduleSkipReason::NoAssignableRange)?;

        let range = self.queue[index].clone();
        // Bound this peer's share of the global byte budget so one fast peer
        // cannot reserve the whole window and starve the others.
        let peer_reserved: u64 = peer
            .outstanding
            .iter()
            .map(|outstanding| outstanding.reserved_bytes())
            .sum();
        let peer_headroom = per_peer_byte_cap.saturating_sub(peer_reserved);
        let max_bytes = budget
            .available()
            .min(u64::from(peer.max_response_bytes.max(1)))
            .min(peer_headroom);
        if max_bytes == 0 {
            return Err(if peer_headroom == 0 {
                ScheduleSkipReason::PeerByteCapExhausted
            } else {
                ScheduleSkipReason::BudgetExhausted
            });
        }
        let max_count = peer.max_blocks_per_response.max(1);
        let mut estimated_bytes = 0u64;
        let mut selected = Vec::new();

        for block in range.blocks.iter().take(max_count as usize) {
            let next_bytes = estimated_bytes.saturating_add(block.estimated_bytes);
            if next_bytes > max_bytes {
                break;
            }
            estimated_bytes = next_bytes;
            selected.push(*block);
        }

        if selected.is_empty() {
            return Err(ScheduleSkipReason::FirstBlockExceedsByteLimit);
        }
        if !budget.try_reserve(estimated_bytes) {
            return Err(ScheduleSkipReason::ReserveFailed);
        }

        let count =
            u32::try_from(selected.len()).map_err(|_| ScheduleSkipReason::RequestCountOverflow)?;
        let request = BlockRangeRequest {
            start_height: selected[0].height,
            count,
            anchor_hash: selected[0].hash,
            estimated_bytes,
            expected_hashes: selected
                .iter()
                .map(|block| (block.height, block.hash))
                .collect(),
            expected_bytes: selected
                .iter()
                .map(|block| (block.height, block.estimated_bytes))
                .collect(),
        };

        if selected.len() == range.blocks.len() {
            if self
                .assigned
                .get(&range.key())
                .is_some_and(|peers| peers.len() + 1 >= self.fanout)
            {
                self.queue.remove(index);
            }
        } else if let Some(queued) = self.queue.get_mut(index) {
            queued.blocks.drain(..selected.len());
        }

        self.assigned
            .entry(request.key())
            .or_default()
            .insert(peer_id.clone());
        Ok(request)
    }

    pub(super) fn retry(&mut self, range: BlockRangeRequest) {
        if self.is_covered(range.start_height, range.end_height()) {
            return;
        }
        self.clear_assignment(&range);
        let retry_range = BlockRange {
            blocks: (0..range.count)
                .filter_map(|offset| {
                    range
                        .start_height
                        .0
                        .checked_add(offset)
                        .map(|raw| ScheduledBlock {
                            height: block::Height(raw),
                            hash: range
                                .expected_hash(block::Height(raw))
                                .unwrap_or(block::Hash([0; 32])),
                            estimated_bytes: range
                                .estimated_bytes_for_height(block::Height(raw))
                                .unwrap_or(range.estimated_bytes / u64::from(range.count)),
                        })
                })
                .collect(),
        };
        for segment in self.uncovered_segments(retry_range).into_iter().rev() {
            self.queue.push_front(segment);
        }
    }

    #[cfg(test)]
    pub(super) fn complete(&mut self, range: &BlockRangeRequest, budget: &mut ByteBudget) {
        budget.release(range.estimated_bytes);
        self.clear_assignment(range);
        for (height, _) in &range.expected_hashes {
            self.mark_height_covered(*height);
        }
    }

    #[cfg(test)]
    pub(super) fn timeout(&mut self, range: BlockRangeRequest, budget: &mut ByteBudget) {
        budget.release(range.estimated_bytes);
        self.retry(range);
    }

    pub(super) fn forget_peer(&mut self, peer: &ZakuraPeerId) {
        for peers in self.assigned.values_mut() {
            peers.remove(peer);
        }
    }

    pub(super) fn clear_assignment(&mut self, range: &BlockRangeRequest) {
        self.assigned.remove(&range.key());
    }

    pub(super) fn mark_height_covered(&mut self, height: block::Height) {
        self.mark_covered_interval(CoveredRange {
            start: height,
            end: height,
        });
        self.prune_covered();
    }

    pub(super) fn clear_covered_from(&mut self, from: block::Height) {
        let previous = previous_height(from);
        self.covered.retain_mut(|covered| {
            if covered.start >= from {
                return false;
            }
            if covered.end >= from {
                if let Some(previous) = previous {
                    covered.end = previous;
                } else {
                    return false;
                }
            }
            true
        });
    }

    #[cfg(test)]
    pub(super) fn release_cancelled(&mut self, budget: &mut ByteBudget) {
        self.assigned.clear();
        self.queue.clear();
        let reserved = budget.reserved();
        budget.release(reserved);
    }

    #[cfg(test)]
    pub(super) fn assigned_range_count(&self) -> usize {
        self.assigned.len()
    }

    /// Diagnostics: number of queued (not-yet-assigned-to-fanout) ranges.
    pub(super) fn queued_range_count(&self) -> usize {
        self.queue.len()
    }

    /// Diagnostics: number of queued (not-yet-assigned-to-fanout) block heights.
    pub(super) fn queued_block_count(&self) -> usize {
        self.queue.iter().map(|range| range.blocks.len()).sum()
    }

    /// Diagnostics: lowest start height across all queued ranges.
    ///
    /// A frozen download floor with a non-empty `needed` set but a
    /// `queued_min_start` above the gap means the gap range was rejected by
    /// `ensure` (covered/queue/assigned overlap) rather than starved.
    pub(super) fn queued_min_start(&self) -> Option<block::Height> {
        self.queue.iter().map(|range| range.start_height()).min()
    }

    /// Diagnostics: number of distinct assigned range keys (including any left
    /// behind by `forget_peer` with an empty peer set).
    pub(super) fn assigned_key_count(&self) -> usize {
        self.assigned.len()
    }

    /// Diagnostics: highest end height across all covered intervals.
    pub(super) fn covered_max_end(&self) -> Option<block::Height> {
        self.covered.iter().map(|covered| covered.end).max()
    }

    fn ensure(&mut self, range: BlockRange) {
        if range.blocks.is_empty() {
            return;
        }
        for range in self.uncovered_segments(range) {
            let blocked = self
                .queue
                .iter()
                .map(|queued| CoveredRange {
                    start: queued.start_height(),
                    end: queued.end_height(),
                })
                .chain(self.assigned.keys().map(|assigned| CoveredRange {
                    start: assigned.start,
                    end: assigned.end,
                }))
                .collect::<Vec<_>>();

            for segment in Self::uncovered_segments_for(&blocked, range) {
                self.queue.push_back(segment);
            }
        }
    }

    fn estimate_bytes(&self, estimate: BlockSizeEstimate) -> u64 {
        let hinted = match estimate {
            BlockSizeEstimate::Confirmed(size) | BlockSizeEstimate::Advertised(size) => {
                u64::from(size)
            }
            BlockSizeEstimate::Unknown => self.ewma_fallback_bytes,
        };
        hinted
            .max(self.floor_estimate_bytes)
            .min(block::MAX_BLOCK_BYTES)
    }

    fn mark_covered_interval(&mut self, mut interval: CoveredRange) {
        let mut merged = Vec::with_capacity(self.covered.len().saturating_add(1));
        let mut inserted = false;
        for covered in self.covered.drain(..) {
            if covered.end.0.saturating_add(1) < interval.start.0 {
                merged.push(covered);
            } else if interval.end.0.saturating_add(1) < covered.start.0 {
                if !inserted {
                    merged.push(interval);
                    inserted = true;
                }
                merged.push(covered);
            } else {
                interval.start = interval.start.min(covered.start);
                interval.end = interval.end.max(covered.end);
            }
        }
        if !inserted {
            merged.push(interval);
        }
        self.covered = merged;
    }

    fn prune_covered(&mut self) {
        let mut queue = VecDeque::with_capacity(self.queue.len());
        for range in self.queue.drain(..) {
            queue.extend(Self::uncovered_segments_for(&self.covered, range));
        }
        self.queue = queue;
        let covered = &self.covered;
        self.assigned.retain(|range, _| {
            !covered
                .iter()
                .any(|covered| covered.start <= range.start && covered.end >= range.end)
        });
    }

    fn is_covered(&self, start: block::Height, end: block::Height) -> bool {
        self.covered
            .iter()
            .any(|covered| covered.start <= start && covered.end >= end)
    }

    fn can_assign_peer_to_range(&self, peer_id: &ZakuraPeerId, range: &BlockRange) -> bool {
        let mut assigned_peers = HashSet::new();
        for (assigned, peers) in &self.assigned {
            if assigned.start <= range.end_height() && assigned.end >= range.start_height() {
                assigned_peers.extend(peers.iter());
            }
        }

        assigned_peers.len() < self.fanout && !assigned_peers.contains(peer_id)
    }

    fn uncovered_segments(&self, range: BlockRange) -> Vec<BlockRange> {
        Self::uncovered_segments_for(&self.covered, range)
    }

    fn uncovered_segments_for(covered: &[CoveredRange], range: BlockRange) -> Vec<BlockRange> {
        let mut segments = Vec::new();
        let mut current = Vec::new();
        for block in range.blocks {
            let is_covered = covered
                .iter()
                .any(|covered| covered.start <= block.height && covered.end >= block.height);
            if is_covered {
                if !current.is_empty() {
                    segments.push(BlockRange { blocks: current });
                    current = Vec::new();
                }
            } else {
                current.push(block);
            }
        }
        if !current.is_empty() {
            segments.push(BlockRange { blocks: current });
        }
        segments
    }
}

#[derive(Clone, Debug)]
struct BlockRange {
    blocks: Vec<ScheduledBlock>,
}

impl BlockRange {
    fn start_height(&self) -> block::Height {
        self.blocks
            .first()
            .expect("block range is never empty")
            .height
    }

    fn end_height(&self) -> block::Height {
        self.blocks
            .last()
            .expect("block range is never empty")
            .height
    }

    fn key(&self) -> BlockRangeKey {
        BlockRangeKey {
            start: self.start_height(),
            end: self.end_height(),
        }
    }

    fn matches_needed(&self, needed: &HashMap<block::Height, block::Hash>) -> bool {
        self.blocks
            .iter()
            .all(|block| needed.get(&block.height) == Some(&block.hash))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ScheduledBlock {
    height: block::Height,
    hash: block::Hash,
    estimated_bytes: u64,
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
struct BlockRangeKey {
    start: block::Height,
    end: block::Height,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct CoveredRange {
    start: block::Height,
    end: block::Height,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BlockRangeRequest {
    pub(super) start_height: block::Height,
    pub(super) count: u32,
    pub(super) anchor_hash: block::Hash,
    pub(super) estimated_bytes: u64,
    pub(super) expected_hashes: Vec<(block::Height, block::Hash)>,
    pub(super) expected_bytes: Vec<(block::Height, u64)>,
}

impl BlockRangeRequest {
    pub(super) fn end_height(&self) -> block::Height {
        height_after_count(self.start_height, self.count)
            .and_then(previous_height)
            .expect("range request count is non-zero")
    }

    pub(super) fn contains(&self, height: block::Height) -> bool {
        self.start_height <= height && height <= self.end_height()
    }

    pub(super) fn expected_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.expected_hashes
            .iter()
            .find_map(|(known_height, hash)| (*known_height == height).then_some(*hash))
    }

    pub(super) fn estimated_bytes_for_height(&self, height: block::Height) -> Option<u64> {
        self.expected_bytes
            .iter()
            .find_map(|(known_height, bytes)| (*known_height == height).then_some(*bytes))
    }

    pub(super) fn single_height_retry(&self, height: block::Height) -> Option<Self> {
        let hash = self.expected_hash(height)?;
        let estimated_bytes = self.estimated_bytes_for_height(height)?;
        Some(Self {
            start_height: height,
            count: 1,
            anchor_hash: hash,
            estimated_bytes,
            expected_hashes: vec![(height, hash)],
            expected_bytes: vec![(height, estimated_bytes)],
        })
    }

    pub(super) fn matches_needed(&self, needed: &HashMap<block::Height, block::Hash>) -> bool {
        self.expected_hashes
            .iter()
            .all(|(height, hash)| needed.get(height) == Some(hash))
    }

    fn key(&self) -> BlockRangeKey {
        BlockRangeKey {
            start: self.start_height,
            end: self.end_height(),
        }
    }
}
