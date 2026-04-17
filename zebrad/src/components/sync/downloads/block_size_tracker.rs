//! Tracks recent block sizes for adaptive batch download sizing.

use std::{collections::VecDeque, sync::Arc};

use tokio::sync::mpsc;

use zebra_chain::{block::Block, serialization::ZcashSerialize};

/// Cloneable handle for reporting block sizes from spawned download tasks.
///
/// Bundles the channel sender with the sampling offset so callers only need
/// to clone a single value before moving into an async block.
#[derive(Clone, Debug)]
pub struct BlockSizeTrackerSender {
    sender: mpsc::UnboundedSender<usize>,
    sample_offset: u32,
}

impl BlockSizeTrackerSender {
    /// Samples the serialized size of `block` if it falls on the sampling interval.
    ///
    /// Only every 100th block (at the tracker's random offset) is measured, to
    /// keep serialization overhead low.
    pub fn sample(&self, block: &Arc<Block>) {
        if let Some(height) = block.coinbase_height() {
            if (height.0 + self.sample_offset).is_multiple_of(100) {
                let _ = self.sender.send(block.zcash_serialized_size());
            }
        }
    }
}

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

    /// Returns a cloneable sender handle for reporting block sizes from spawned tasks.
    pub fn sender(&self) -> BlockSizeTrackerSender {
        BlockSizeTrackerSender {
            sender: self.sender.clone(),
            sample_offset: self.sample_offset,
        }
    }

    /// Sends a raw size sample directly, bypassing height-based sampling.
    ///
    /// Used in tests to feed known sizes into the tracker.
    #[cfg(test)]
    fn send_sample(&self, size: usize) {
        self.sender.send(size).expect("receiver is held by self");
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
mod tests;
