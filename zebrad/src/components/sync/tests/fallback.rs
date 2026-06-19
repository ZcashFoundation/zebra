//! Tests for the Zakura body-sync stall watchdog
//! ([`ChainSync::bootstrap_genesis_then_pause`]).
//!
//! These exercise the pure decision function [`zakura_block_sync_stalled`] directly,
//! so they are deterministic and need no clock, services, or live `ChainTip`.

use zebra_chain::block::Height;

use super::super::{zakura_block_sync_stalled, ZakuraStallTracker};

/// The original height-only rule, reproduced here only to demonstrate the F-88602
/// hole: any increase in the verified tip — including a gossip-trickled block —
/// resets the idle counter, so the watchdog never falls back.
fn legacy_stalled(
    last_height: &mut Option<Height>,
    idle_polls: &mut u64,
    verified_height: Option<Height>,
    max_idle_polls: u64,
) -> bool {
    if verified_height > *last_height {
        *last_height = verified_height;
        *idle_polls = 0;
        false
    } else {
        *idle_polls += 1;
        *idle_polls >= max_idle_polls
    }
}

/// A peer trickling next-height blocks over gossip bumps the verified tip without
/// Zakura block sync running. The old height-only rule treats that as health and
/// never falls back (the bug); the new rule sees the gap to the network frontier
/// never closing and falls back.
#[test]
fn gossip_trickle_does_not_suppress_fallback() {
    let max_idle_polls = 5;

    // The frontier sits far ahead and advances in lockstep with each gossiped block,
    // so the gap stays pinned at 1_000: the node is materially behind the whole time.
    let mut verified = 0u32;
    let mut header = 1_000u32;

    let mut legacy_last = Some(Height(verified));
    let mut legacy_idle = 0u64;
    let mut tracker = ZakuraStallTracker::new(Some(Height(verified)));

    let mut legacy_fell_back = false;
    let mut new_fell_back = false;
    for _ in 0..(max_idle_polls * 4) {
        verified += 1;
        header += 1;
        legacy_fell_back |= legacy_stalled(
            &mut legacy_last,
            &mut legacy_idle,
            Some(Height(verified)),
            max_idle_polls,
        );
        new_fell_back |= zakura_block_sync_stalled(
            &mut tracker,
            Some(Height(verified)),
            Some(Height(header)),
            max_idle_polls,
        );
    }

    assert!(
        !legacy_fell_back,
        "the legacy height-only rule never falls back under gossip trickle — this is the \
         F-88602 bug the new rule must fix"
    );
    assert!(
        new_fell_back,
        "the watchdog must fall back when the verified tip only moves via gossip and the gap \
         to the network frontier never closes"
    );
}

/// A working bulk downloader closing a real gap must keep Zakura sync as the primary
/// path and never fall back.
#[test]
fn real_block_sync_progress_keeps_primary_path() {
    let max_idle_polls = 5;
    let header = 10_000u32;
    let mut tracker = ZakuraStallTracker::new(Some(Height(0)));

    let mut verified = 0u32;
    for _ in 0..60 {
        verified = verified.saturating_add(200);
        assert!(
            !zakura_block_sync_stalled(
                &mut tracker,
                Some(Height(verified)),
                Some(Height(header)),
                max_idle_polls,
            ),
            "healthy bulk sync closing 200 blocks/poll must never fall back"
        );
    }
}

/// A node caught up to the frontier, with gossip keeping it current one block at a
/// time, is healthy and must not fall back.
#[test]
fn near_tip_with_gossip_stays_primary() {
    let max_idle_polls = 3;
    let mut tracker = ZakuraStallTracker::new(Some(Height(100)));

    let mut height = 100u32;
    for _ in 0..20 {
        height += 1;
        assert!(
            !zakura_block_sync_stalled(
                &mut tracker,
                Some(Height(height)),
                Some(Height(height)),
                max_idle_polls,
            ),
            "a node caught up to the frontier must not fall back"
        );
    }
}

/// Steady moderate sync that closes fewer than `ZAKURA_BLOCK_SYNC_MIN_CLOSURE`
/// blocks in a single poll but accumulates across polls must still be credited as
/// progress. Guards against a naive running-min anchor that would re-baseline every
/// idle poll and false-positive a working sync.
#[test]
fn steady_moderate_sync_does_not_false_positive() {
    let max_idle_polls = 5;
    let header = 100_000u32;
    let mut tracker = ZakuraStallTracker::new(Some(Height(0)));

    let mut verified = 0u32;
    let mut fell_back = false;
    for _ in 0..400 {
        verified = verified.saturating_add(50);
        fell_back |= zakura_block_sync_stalled(
            &mut tracker,
            Some(Height(verified)),
            Some(Height(header.max(verified))),
            max_idle_polls,
        );
    }
    assert!(
        !fell_back,
        "sync below the per-poll floor but accumulating closure across polls must not fall back"
    );
}

/// With no network frontier known yet, the watchdog degrades to the original
/// "verified tip moved at all" rule so behavior does not regress before header sync
/// reports a frontier.
#[test]
fn without_header_tip_uses_legacy_tip_moved_rule() {
    let max_idle_polls = 3;
    let mut tracker = ZakuraStallTracker::new(Some(Height(0)));

    // Tip advancing, no frontier known: treated as progress.
    for v in 1..=10u32 {
        assert!(!zakura_block_sync_stalled(
            &mut tracker,
            Some(Height(v)),
            None,
            max_idle_polls,
        ));
    }

    // Tip frozen, no frontier known: idle accrues and it falls back after the window.
    let frozen = Some(Height(10));
    let mut fell_back = false;
    for _ in 0..max_idle_polls {
        fell_back = zakura_block_sync_stalled(&mut tracker, frozen, None, max_idle_polls);
    }
    assert!(
        fell_back,
        "with no frontier and a frozen verified tip, the legacy rule still trips the fallback"
    );
}
