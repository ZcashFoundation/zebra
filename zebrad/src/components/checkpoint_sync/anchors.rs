//! Checkpoint-list anchors and interval size estimates: a gap below 400
//! heights means the interval holds about 32 MiB, otherwise
//! `400 × ewma(bytes/block)`.
//!
//! The checkpoint generator ends an interval when its cumulative size
//! reaches [`MAX_CHECKPOINT_BYTE_COUNT`] *or* its gap reaches
//! [`MAX_CHECKPOINT_HEIGHT_GAP`], whichever comes first
//! (`zebra-utils/src/bin/zebra-checkpoints/main.rs`). So an interval with a
//! gap below the maximum holds between 32 MiB and 32 MiB plus one block, and
//! only the full-gap intervals need an estimate.

use zebra_chain::{block, parameters::checkpoint::list::CheckpointList};
use zebra_consensus::{MAX_CHECKPOINT_BYTE_COUNT, MAX_CHECKPOINT_HEIGHT_GAP};

/// The heights below a peer's advertised tip that the engine never targets.
pub const PEER_TIP_MARGIN: u32 = 100;

/// The initial bytes-per-block estimate for full-gap intervals.
pub const INITIAL_BYTES_PER_BLOCK: u64 = 2 * 1024;

/// The number of committed blocks the bytes-per-block EWMA averages over.
pub const BYTES_PER_BLOCK_HORIZON: u64 = 2_000;

/// The serialized bytes the generator puts in an interval whose gap is below
/// [`MAX_CHECKPOINT_HEIGHT_GAP`].
pub const FULL_INTERVAL_BYTES: u64 = MAX_CHECKPOINT_BYTE_COUNT;

/// The checkpoint list, as anchors for `get-block-range` units.
#[derive(Clone, Debug)]
pub struct Anchors {
    /// The checkpoints, in increasing height order. Never empty: genesis is
    /// always a checkpoint.
    checkpoints: Vec<(block::Height, block::Hash)>,
}

impl Anchors {
    /// The anchors of `list`.
    pub fn new(list: &CheckpointList) -> Self {
        Anchors {
            checkpoints: list.iter_cloned().collect(),
        }
    }

    /// The final checkpoint height.
    pub fn final_height(&self) -> block::Height {
        self.checkpoints
            .last()
            .expect("checkpoint lists always contain genesis")
            .0
    }

    /// The hash of the checkpoint at `height`, if `height` is a checkpoint.
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        self.index_of(height).map(|index| self.checkpoints[index].1)
    }

    /// Whether `height` is a checkpoint height.
    pub fn contains(&self, height: block::Height) -> bool {
        self.index_of(height).is_some()
    }

    /// The highest checkpoint at or below `height`.
    pub fn at_or_below(&self, height: block::Height) -> (block::Height, block::Hash) {
        let index = self
            .checkpoints
            .partition_point(|(checkpoint, _)| *checkpoint <= height);

        // `index > 0`: genesis is a checkpoint, and every height is at least 0.
        self.checkpoints[index - 1]
    }

    /// The lowest checkpoint at or above `height`, if any.
    pub fn at_or_above(&self, height: block::Height) -> Option<(block::Height, block::Hash)> {
        let index = self
            .checkpoints
            .partition_point(|(checkpoint, _)| *checkpoint < height);

        self.checkpoints.get(index).copied()
    }

    /// The number of checkpoint intervals that end in `(low, high]`.
    pub fn intervals_between(&self, low: block::Height, high: block::Height) -> usize {
        if high <= low {
            return 0;
        }

        let above_low = self
            .checkpoints
            .partition_point(|(checkpoint, _)| *checkpoint <= low);
        let through_high = self
            .checkpoints
            .partition_point(|(checkpoint, _)| *checkpoint <= high);

        through_high - above_low
    }

    /// The engine's target for a state at `tip` when peers advertise
    /// `peer_tip`: the highest checkpoint at or below
    /// `min(final checkpoint, peer_tip - 100)`, or `None` if that is not
    /// above `tip`.
    pub fn target(&self, tip: block::Height, peer_tip: block::Height) -> Option<block::Height> {
        let ceiling = block::Height(peer_tip.0.saturating_sub(PEER_TIP_MARGIN));
        let ceiling = ceiling.min(self.final_height());
        let (target, _) = self.at_or_below(ceiling);

        (target > tip).then_some(target)
    }

    /// The checkpoint before the checkpoint at `height`.
    fn previous(&self, height: block::Height) -> Option<block::Height> {
        let index = self.index_of(height)?;
        index.checked_sub(1).map(|index| self.checkpoints[index].0)
    }

    fn index_of(&self, height: block::Height) -> Option<usize> {
        self.checkpoints
            .binary_search_by_key(&height, |(checkpoint, _)| *checkpoint)
            .ok()
    }
}

/// Estimates the serialized size of checkpoint intervals.
#[derive(Clone, Debug)]
pub struct SizeEstimator {
    /// The EWMA of bytes per block over the last
    /// [`BYTES_PER_BLOCK_HORIZON`] committed blocks.
    bytes_per_block: f64,
}

impl Default for SizeEstimator {
    fn default() -> Self {
        SizeEstimator {
            // Exact: a small integer constant.
            bytes_per_block: INITIAL_BYTES_PER_BLOCK as f64,
        }
    }
}

impl SizeEstimator {
    /// The current bytes-per-block estimate.
    pub fn bytes_per_block(&self) -> f64 {
        self.bytes_per_block
    }

    /// Records `blocks` committed blocks of `bytes` serialized bytes in total.
    pub fn observe(&mut self, blocks: u64, bytes: u64) {
        if blocks == 0 {
            return;
        }

        // Lossy casts are fine: these are estimates.
        let alpha = (blocks as f64 / BYTES_PER_BLOCK_HORIZON as f64).min(1.0);
        let sample = bytes as f64 / blocks as f64;
        self.bytes_per_block = self.bytes_per_block * (1.0 - alpha) + sample * alpha;
    }

    /// The estimated size of the blocks `start..=end`, where `end` is the
    /// checkpoint ending the interval that contains them.
    ///
    /// A gap below [`MAX_CHECKPOINT_HEIGHT_GAP`] holds [`FULL_INTERVAL_BYTES`],
    /// pro rata when `start` is inside the interval (a state tip that is not
    /// a checkpoint, or a resumed unit). A full gap holds
    /// `blocks × ewma(bytes/block)`.
    pub fn estimate(&self, anchors: &Anchors, start: block::Height, end: block::Height) -> u64 {
        let blocks = u64::from(end.0.saturating_sub(start.0)) + 1;

        let gap = anchors
            .previous(end)
            .map(|previous| end.0 - previous.0)
            // Genesis is its own interval: one block.
            .unwrap_or(1);

        // `MAX_CHECKPOINT_HEIGHT_GAP` is 400, and `gap` is a height
        // difference: the cast is exact.
        if (gap as usize) < MAX_CHECKPOINT_HEIGHT_GAP {
            let gap = u64::from(gap);
            FULL_INTERVAL_BYTES.saturating_mul(blocks.min(gap)) / gap
        } else {
            // A lossy cast is fine: it is an estimate, of at least one byte per
            // block.
            (self.bytes_per_block.max(1.0) as u64).saturating_mul(blocks)
        }
    }
}

#[cfg(test)]
mod tests {
    use zebra_chain::parameters::Network;

    use super::*;

    #[test]
    fn mainnet_estimates_follow_the_32_mib_rule() {
        let anchors = Anchors::new(&Network::Mainnet.checkpoint_list());
        let estimator = SizeEstimator::default();

        let mut short = 0;
        let mut full = 0;
        for window in anchors.checkpoints.windows(2) {
            let (previous, end) = (window[0].0, window[1].0);
            let gap = end.0 - previous.0;
            assert!(
                gap as usize <= MAX_CHECKPOINT_HEIGHT_GAP,
                "the generator never makes a gap above the maximum"
            );

            let estimate = estimator.estimate(&anchors, previous.next().unwrap(), end);
            if (gap as usize) < MAX_CHECKPOINT_HEIGHT_GAP {
                short += 1;
                assert_eq!(estimate, FULL_INTERVAL_BYTES);
            } else {
                full += 1;
                assert_eq!(estimate, u64::from(gap) * INITIAL_BYTES_PER_BLOCK);
            }
        }

        // The Mainnet list has both kinds (6,766 32-MiB intervals).
        assert!(short > 6_000, "{short} short intervals");
        assert!(full > 5_000, "{full} full intervals");
    }

    #[test]
    fn partial_intervals_are_pro_rata() {
        let anchors = Anchors::new(&Network::Mainnet.checkpoint_list());
        let estimator = SizeEstimator::default();

        let (previous, end) = anchors
            .checkpoints
            .windows(2)
            .map(|w| (w[0].0, w[1].0))
            .find(|(previous, end)| ((end.0 - previous.0) as usize) < MAX_CHECKPOINT_HEIGHT_GAP)
            .expect("Mainnet has short intervals");
        let gap = u64::from(end.0 - previous.0);

        assert_eq!(
            estimator.estimate(&anchors, end, end),
            FULL_INTERVAL_BYTES / gap
        );
        assert_eq!(
            estimator.estimate(&anchors, previous.next().unwrap(), end),
            FULL_INTERVAL_BYTES
        );
    }

    #[test]
    fn estimator_tracks_committed_sizes() {
        let mut estimator = SizeEstimator::default();
        estimator.observe(BYTES_PER_BLOCK_HORIZON, BYTES_PER_BLOCK_HORIZON * 10_000);
        assert_eq!(estimator.bytes_per_block(), 10_000.0);

        estimator.observe(
            BYTES_PER_BLOCK_HORIZON / 2,
            BYTES_PER_BLOCK_HORIZON / 2 * 2_000,
        );
        assert_eq!(estimator.bytes_per_block(), 6_000.0);
    }

    #[test]
    fn targets_are_checkpoints_below_the_peer_tip() {
        let anchors = Anchors::new(&Network::Mainnet.checkpoint_list());
        let final_height = anchors.final_height();

        let target = anchors
            .target(block::Height(0), block::Height(1_000_000))
            .unwrap();
        assert!(anchors.contains(target));
        assert!(target.0 <= 1_000_000 - PEER_TIP_MARGIN);
        assert!(
            anchors.at_or_above(target.next().unwrap()).unwrap().0
                > block::Height(1_000_000 - PEER_TIP_MARGIN)
        );

        assert_eq!(
            anchors.target(block::Height(0), block::Height::MAX),
            Some(final_height)
        );
        assert_eq!(anchors.target(final_height, block::Height::MAX), None);
        assert_eq!(anchors.target(block::Height(0), block::Height(50)), None);
    }

    #[test]
    fn interval_counts() {
        let anchors = Anchors::new(&Network::Mainnet.checkpoint_list());
        let (c1, _) = anchors.checkpoints[1];
        let (c2, _) = anchors.checkpoints[2];
        let (c5, _) = anchors.checkpoints[5];

        assert_eq!(anchors.intervals_between(block::Height(0), c5), 5);
        assert_eq!(anchors.intervals_between(c1, c5), 4);
        assert_eq!(anchors.intervals_between(c5, c1), 0);
        assert_eq!(anchors.at_or_below(c5.next().unwrap()).0, c5);
        assert_eq!(anchors.at_or_above(c1.next().unwrap()).unwrap().0, c2);
        assert_eq!(anchors.at_or_above(c1).unwrap().0, c1);
    }
}
