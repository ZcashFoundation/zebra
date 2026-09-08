//! Unit tests for [`FindResponseStallTracker`].

use super::*;

/// Distinguishes an unclassified event from feedback that is still pending.
#[test]
fn observer_distinguishes_unclassified_feedback() {
    let (feedback, mut observer) = FindResponseFeedback::new_for_test();

    assert_eq!(
        observer.try_outcome(),
        Err(mpsc::error::TryRecvError::Empty)
    );

    drop(feedback);

    assert_eq!(observer.try_outcome(), Ok(None));
    assert_eq!(
        observer.try_outcome(),
        Err(mpsc::error::TryRecvError::Disconnected)
    );
}

fn test_addr(last_octet: u8) -> PeerSocketAddr {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(127, 0, 0, last_octet),
        8233,
    ))
    .into()
}

#[test]
fn disconnects_after_threshold() {
    let mut tracker = FindResponseStallTracker::new();
    let addr = test_addr(1);

    assert!(!tracker.record_stall(addr));
    assert!(!tracker.record_stall(addr));

    // Third stall: at threshold.
    assert!(tracker.record_stall(addr));

    // Entry cleared on threshold — next stall starts fresh.
    assert!(!tracker.record_stall(addr));
}

#[test]
fn clear_resets_count() {
    let mut tracker = FindResponseStallTracker::new();
    let addr = test_addr(1);

    assert!(!tracker.record_stall(addr));
    assert!(!tracker.record_stall(addr));

    tracker.clear(addr);

    // Back to zero: needs a full threshold's worth of stalls again.
    assert!(!tracker.record_stall(addr));
    assert!(!tracker.record_stall(addr));
    assert!(tracker.record_stall(addr));
}

#[test]
fn independent_per_peer() {
    let mut tracker = FindResponseStallTracker::new();
    let addr_a = test_addr(1);
    let addr_b = test_addr(2);

    assert!(!tracker.record_stall(addr_a));
    assert!(!tracker.record_stall(addr_a));
    assert!(!tracker.record_stall(addr_b));
    assert!(tracker.record_stall(addr_a));

    assert!(!tracker.record_stall(addr_b));
    assert!(tracker.record_stall(addr_b));
}

mod feedback;
