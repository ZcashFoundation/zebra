//! Tracks recent block sizes for adaptive batch download sizing.

use std::collections::VecDeque;

use tokio::sync::mpsc;

/// Target total size for a batch of downloaded blocks, in bytes.
const BATCH_TARGET_SIZE: usize = 1_000_000;

/// Maximum number of block hashes in a single batched download request.
pub const MAX_BATCH_SIZE: usize = 16;

/// Number of recent block sizes to keep for computing the rolling average.
const MAX_RECENT_BLOCK_SIZE_SAMPLES: usize = 100;

/// Tracks recent block sizes to dynamically compute batch download sizes.
///
/// Spawned tasks report block sizes through a channel sender. The receiver is
/// drained on each call to [`recommended_batch_size`](Self::recommended_batch_size),
/// which returns a batch size tuned to the peer's send buffer limit.
///
/// To reduce overhead, only a sample of blocks (every 100th, at a random
/// offset) compute their serialized size.
#[derive(Debug)]
pub struct BlockSizeTracker {
    sender: mpsc::UnboundedSender<usize>,
    receiver: mpsc::UnboundedReceiver<usize>,
    samples: VecDeque<usize>,
    total: usize,
    /// Random offset chosen once at construction. A block reports its size
    /// only when `(height + sample_offset) % 100 == 0`.
    sample_offset: u32,
}

impl BlockSizeTracker {
    /// Creates a new tracker with an empty sample window and a random sampling offset.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver,
            samples: VecDeque::new(),
            total: 0,
            sample_offset: rand::random::<u32>() % 100,
        }
    }

    /// Returns a sender that spawned tasks use to report block sizes.
    pub fn sender(&self) -> mpsc::UnboundedSender<usize> {
        self.sender.clone()
    }

    /// Returns the sampling offset for spawned tasks.
    pub fn sample_offset(&self) -> u32 {
        self.sample_offset
    }

    /// Returns the recommended batch size based on recently observed block sizes.
    ///
    /// Drains pending reports, then computes
    /// `BATCH_TARGET_SIZE / avg_block_size` clamped to `[1, MAX_BATCH_SIZE]`.
    /// Returns `MAX_BATCH_SIZE` if no sizes have been recorded yet.
    pub fn recommended_batch_size(&mut self) -> usize {
        while let Ok(size) = self.receiver.try_recv() {
            self.samples.push_back(size);
            self.total += size;
            if self.samples.len() > MAX_RECENT_BLOCK_SIZE_SAMPLES {
                self.total -= self.samples.pop_front().unwrap_or(0);
            }
        }

        if self.samples.is_empty() {
            return MAX_BATCH_SIZE;
        }

        let avg_size = self.total / self.samples.len();
        if avg_size == 0 {
            return MAX_BATCH_SIZE;
        }

        // Use 4/5 of the raw ratio as a safety margin to account for block
        // size variance and avoid partial responses from peers.
        ((4 * BATCH_TARGET_SIZE) / (5 * avg_size)).clamp(1, MAX_BATCH_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tracker_returns_max_batch_size() {
        let mut tracker = BlockSizeTracker::new();
        assert_eq!(tracker.recommended_batch_size(), MAX_BATCH_SIZE);
    }

    #[test]
    fn small_blocks_yield_max_batch_size() {
        let mut tracker = BlockSizeTracker::new();
        // 1KB blocks: 1_000_000 / 1_000 * 4/5 = 800, clamped to 16
        for _ in 0..10 {
            tracker.sender().send(1_000).unwrap();
        }
        assert_eq!(tracker.recommended_batch_size(), MAX_BATCH_SIZE);
    }

    #[test]
    fn large_blocks_yield_small_batch_size() {
        let mut tracker = BlockSizeTracker::new();
        // 800KB blocks: 1_000_000 / 800_000 * 4/5 = 1.0, clamped to 1
        for _ in 0..10 {
            tracker.sender().send(800_000).unwrap();
        }
        assert_eq!(tracker.recommended_batch_size(), 1);
    }

    #[test]
    fn medium_blocks_yield_intermediate_batch_size() {
        let mut tracker = BlockSizeTracker::new();
        // 200KB blocks: 1_000_000 / 200_000 * 4/5 = 4
        for _ in 0..10 {
            tracker.sender().send(200_000).unwrap();
        }
        assert_eq!(tracker.recommended_batch_size(), 4);
    }

    #[test]
    fn rolling_window_evicts_old_samples() {
        let mut tracker = BlockSizeTracker::new();
        // Push 100 large samples (800KB)
        for _ in 0..MAX_RECENT_BLOCK_SIZE_SAMPLES {
            tracker.sender().send(800_000).unwrap();
        }
        // Drain them
        assert_eq!(tracker.recommended_batch_size(), 1);

        // Push 100 small samples (1KB) — old large samples should be evicted
        for _ in 0..MAX_RECENT_BLOCK_SIZE_SAMPLES {
            tracker.sender().send(1_000).unwrap();
        }
        assert_eq!(tracker.recommended_batch_size(), MAX_BATCH_SIZE);
    }

    #[test]
    fn zero_size_blocks_return_max() {
        let mut tracker = BlockSizeTracker::new();
        for _ in 0..10 {
            tracker.sender().send(0).unwrap();
        }
        assert_eq!(tracker.recommended_batch_size(), MAX_BATCH_SIZE);
    }
}
