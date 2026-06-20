use super::{state::*, *};

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

/// A contiguous block-range request issued to one peer and tracked in its
/// `outstanding` set. Built by the reactor's per-peer issuance path from a chunk
/// taken out of the [`WorkQueue`](super::work_queue::WorkQueue).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BlockRangeRequest {
    pub(super) start_height: block::Height,
    pub(super) count: u32,
    pub(super) anchor_hash: block::Hash,
    /// The reserved worst-case byte total for this request (released on
    /// timeout/disconnect/send-failure). Distinct from the per-height size
    /// estimates in `expected_bytes`.
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
}
