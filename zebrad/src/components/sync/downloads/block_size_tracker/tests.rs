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
        tracker.send_sample(1_000);
    }
    assert_eq!(tracker.recommended_batch_size(), MAX_BATCH_SIZE);
}

#[test]
fn large_blocks_yield_small_batch_size() {
    let mut tracker = BlockSizeTracker::new();
    // 800KB blocks: 1_000_000 / 800_000 * 4/5 = 1.0, clamped to 1
    for _ in 0..10 {
        tracker.send_sample(800_000);
    }
    assert_eq!(tracker.recommended_batch_size(), 1);
}

#[test]
fn medium_blocks_yield_intermediate_batch_size() {
    let mut tracker = BlockSizeTracker::new();
    // 200KB blocks: 1_000_000 / 200_000 * 4/5 = 4
    for _ in 0..10 {
        tracker.send_sample(200_000);
    }
    assert_eq!(tracker.recommended_batch_size(), 4);
}

#[test]
fn rolling_window_evicts_old_samples() {
    let mut tracker = BlockSizeTracker::new();
    // Push 100 large samples (800KB)
    for _ in 0..MAX_RECENT_BLOCK_SIZE_SAMPLES {
        tracker.send_sample(800_000);
    }
    // Drain them
    assert_eq!(tracker.recommended_batch_size(), 1);

    // Push 100 small samples (1KB) — old large samples should be evicted
    for _ in 0..MAX_RECENT_BLOCK_SIZE_SAMPLES {
        tracker.send_sample(1_000);
    }
    assert_eq!(tracker.recommended_batch_size(), MAX_BATCH_SIZE);
}

#[test]
fn zero_size_blocks_return_max() {
    let mut tracker = BlockSizeTracker::new();
    for _ in 0..10 {
        tracker.send_sample(0);
    }
    assert_eq!(tracker.recommended_batch_size(), MAX_BATCH_SIZE);
}
