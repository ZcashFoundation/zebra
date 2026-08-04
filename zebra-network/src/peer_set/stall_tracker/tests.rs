//! Unit tests for [`FindResponseStallTracker`].

use super::*;

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

/// Checks that cloned [`FindResponseFeedback`] handles classify only once.
#[test]
fn find_response_feedback_is_one_shot() {
    let addr = test_addr(1);
    let request_id = FindRequestId::from(1);
    let (feedback_tx, mut feedback_rx) = tokio::sync::mpsc::unbounded_channel();

    let useful_feedback = FindResponseFeedback::new(addr, request_id, feedback_tx.clone());
    let duplicate_feedback = useful_feedback.clone();

    useful_feedback.mark_useful();
    duplicate_feedback.mark_stalled();

    assert_eq!(
        feedback_rx.try_recv(),
        Ok(FindResponseEvent::new(
            addr,
            request_id,
            FindResponseOutcome::Useful,
        )),
    );
    assert_eq!(
        feedback_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty),
        "a cloned feedback handle must not classify the same response twice",
    );
}

/// Checks that dropping unclassified [`FindResponseFeedback`] reports no judgment.
///
/// Consumers can abandon responses on local error or cancellation paths, which
/// must not count against the responding peer.
#[test]
fn dropped_find_response_feedback_reports_unclassified() {
    let addr = test_addr(1);
    let request_id = FindRequestId::from(1);
    let (feedback_tx, mut feedback_rx) = tokio::sync::mpsc::unbounded_channel();

    drop(FindResponseFeedback::new(addr, request_id, feedback_tx));

    assert_eq!(
        feedback_rx.try_recv(),
        Ok(FindResponseEvent::new(
            addr,
            request_id,
            FindResponseOutcome::Unclassified,
        )),
        "dropping the final unclassified handle must report no judgment",
    );
}
