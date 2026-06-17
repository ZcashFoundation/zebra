//! Shared per-session protections for Zakura services.
//!
//! [`SessionGuard`] is the single home for the protections that wrap a peer
//! session: the allowed inbound message-type filter, the optional byte budget,
//! and the per-peer semantic meters. It reuses the existing limiter primitives
//! rather than inventing new limiter math.
//!
//! Boundary note (do not double-count). The transport's per-connection,
//! per-stream-kind message-rate `TokenBucket` and the oversize check already
//! run in the transport stream worker *before* a frame reaches the service.
//! `SessionGuard` therefore owns only the **service-specific** protections
//! (allowed-types, byte budget, per-peer semantic meters); the transport keeps
//! its connection-global count bucket exactly as-is. Document this split at the
//! [`SessionGuard::new`] call site.

use super::Frame;

/// Byte-rate reservation budget for inflight stream payloads.
///
/// Promoted here from `block_sync/state.rs` so byte-rate protection is reusable
/// across services; only block_sync currently passes `Some(..)` to a guard.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ByteBudget {
    max_bytes: u64,
    reserved_bytes: u64,
}

impl ByteBudget {
    pub(crate) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            reserved_bytes: 0,
        }
    }

    pub(crate) fn available(self) -> u64 {
        self.max_bytes.saturating_sub(self.reserved_bytes)
    }

    pub(crate) fn reserved(self) -> u64 {
        self.reserved_bytes
    }

    pub(crate) fn try_reserve(&mut self, bytes: u64) -> bool {
        if bytes == 0 || bytes > self.available() {
            return false;
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(bytes);
        true
    }

    pub(crate) fn release(&mut self, bytes: u64) {
        self.reserved_bytes = self.reserved_bytes.saturating_sub(bytes);
    }
}

/// Outcome of admitting one inbound frame through a [`SessionGuard`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Admit {
    /// The frame is admitted for processing.
    Pass,
    /// The frame is dropped but the peer is kept (rate/budget back-pressure).
    Throttle,
    /// The peer violated a protocol-level protection and should be disconnected.
    Reject(&'static str),
}

/// Per-peer semantic meters (status spam, new-block spam, ...).
///
/// Minimal in Phase 0: a pass-through that admits everything. The real
/// per-service semantic meters (the `RateMeter`s currently in the per-peer
/// state) are moved here in a later migration phase.
#[derive(Debug)]
pub(crate) struct PeerMeters;

impl PeerMeters {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn try_take(&mut self, _message_type: u8) -> bool {
        true
    }
}

/// The single home for the protections that wrap one peer session.
#[derive(Debug)]
pub(crate) struct SessionGuard {
    /// Allowed inbound message types for this stream kind.
    allowed: &'static [u8],
    /// Maximum reassembled message size in bytes.
    max_bytes: u32,
    /// Optional byte budget; `None` for services without byte-rate protection.
    byte_budget: Option<ByteBudget>,
    /// Per-peer semantic meters.
    meters: PeerMeters,
}

impl SessionGuard {
    /// Build a session guard for one peer stream.
    ///
    /// Boundary note (do not double-count): the transport already applies the
    /// per-connection, per-stream-kind message-rate bucket and the oversize cap
    /// before frames reach this guard. Pass `Some(..)` for `byte_budget` only
    /// when this service owns byte-rate protection (block_sync); header_sync and
    /// others pass `None`.
    ///
    /// Phase 1 header_sync uses [`SessionGuard::oversize_only`] (the allowed-type
    /// filter stays off so the decode stage remains the sole validity arbiter);
    /// the explicit-`allowed` constructor is consumed when block_sync/discovery
    /// move their allowed-type lists onto the guard in later phases.
    #[allow(dead_code)] // consumed when block_sync/discovery move their type filters onto the guard
    pub(crate) fn new(
        allowed: &'static [u8],
        max_bytes: u32,
        byte_budget: Option<ByteBudget>,
    ) -> Self {
        Self {
            allowed,
            max_bytes,
            byte_budget,
            meters: PeerMeters::new(),
        }
    }

    /// Build a guard that applies only the oversize cap and admits every type.
    ///
    /// This is the behavior-preserving configuration for a service that has not
    /// yet moved its allowed-type filter and per-peer semantic meters behind the
    /// guard: the decode stage remains the sole arbiter of message validity, so
    /// the exact same wire events fire as before the lift. The allowed-type
    /// filter (`ALL_TYPES`) and the meters are wired per-service in later phases;
    /// `byte_budget` stays `None` because only block_sync owns byte-rate
    /// protection.
    pub(crate) fn oversize_only(max_bytes: u32) -> Self {
        // An empty `allowed` slot would reject every type; `ALL_TYPES` admits
        // every `u8` discriminator so type validity is left to the decode stage.
        const ALL_TYPES: &[u8] = &{
            let mut all = [0u8; 256];
            let mut ty = 0usize;
            while ty < all.len() {
                // `ty` ranges 0..=255 so the `as u8` truncation is exact.
                all[ty] = ty as u8;
                ty += 1;
            }
            all
        };
        Self {
            allowed: ALL_TYPES,
            max_bytes,
            byte_budget: None,
            meters: PeerMeters::new(),
        }
    }

    /// Admit one inbound frame through the service-specific protections.
    pub(crate) fn admit(&mut self, frame: &Frame) -> Admit {
        // `message_type` is a wire `u16`; allowed message types are single
        // bytes, so a value that does not fit in a `u8` is by definition not an
        // allowed type and is treated as a protocol violation.
        let Ok(ty) = u8::try_from(frame.message_type) else {
            return Admit::Reject("bad type");
        };
        if !self.allowed.contains(&ty) {
            return Admit::Reject("disallowed type");
        }
        // `max_bytes` is a `u32`; widen to `usize` for the payload-length
        // comparison (`usize` is at least 32 bits on supported targets).
        if frame.payload.len() > self.max_bytes as usize {
            return Admit::Reject("oversize");
        }
        if let Some(budget) = &mut self.byte_budget {
            // Payload length fits in `u64` on all supported targets.
            if !budget.try_reserve(frame.payload.len() as u64) {
                return Admit::Throttle;
            }
        }
        if !self.meters.try_take(ty) {
            return Admit::Throttle;
        }
        Admit::Pass
    }

    /// Return reserved bytes to the byte budget once a message is processed.
    ///
    /// Unused until a service with a byte budget (block_sync) moves onto the
    /// guard; header_sync passes `byte_budget: None`, so it never calls this.
    #[allow(dead_code)] // consumed when block_sync moves its byte budget onto the guard
    pub(crate) fn release(&mut self, bytes: u64) {
        if let Some(budget) = &mut self.byte_budget {
            budget.release(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWED: &[u8] = &[1, 2];

    fn frame(message_type: u16, payload_len: usize) -> Frame {
        Frame {
            message_type,
            flags: 0,
            payload: vec![0u8; payload_len],
        }
    }

    #[test]
    fn byte_budget_reserves_and_releases() {
        let mut budget = ByteBudget::new(1_000);
        assert_eq!(budget.available(), 1_000);
        assert!(budget.try_reserve(400));
        assert_eq!(budget.reserved(), 400);
        assert_eq!(budget.available(), 600);
        // Zero-byte and over-budget reservations are rejected without mutation.
        assert!(!budget.try_reserve(0));
        assert!(!budget.try_reserve(601));
        assert_eq!(budget.reserved(), 400);
        budget.release(400);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn admit_rejects_disallowed_type() {
        let mut guard = SessionGuard::new(ALLOWED, 1_024, None);
        assert_eq!(guard.admit(&frame(99, 0)), Admit::Reject("disallowed type"));
    }

    #[test]
    fn admit_rejects_non_u8_type() {
        let mut guard = SessionGuard::new(ALLOWED, 1_024, None);
        assert_eq!(guard.admit(&frame(0x0100, 0)), Admit::Reject("bad type"));
    }

    #[test]
    fn admit_rejects_oversize() {
        let mut guard = SessionGuard::new(ALLOWED, 4, None);
        assert_eq!(guard.admit(&frame(1, 5)), Admit::Reject("oversize"));
    }

    #[test]
    fn admit_throttles_when_budget_exhausted() {
        let mut guard = SessionGuard::new(ALLOWED, 1_024, Some(ByteBudget::new(8)));
        // First frame reserves 8 bytes and passes.
        assert_eq!(guard.admit(&frame(1, 8)), Admit::Pass);
        // Second frame cannot reserve and is throttled (peer kept).
        assert_eq!(guard.admit(&frame(1, 8)), Admit::Throttle);
        // Releasing the budget admits the next frame again.
        guard.release(8);
        assert_eq!(guard.admit(&frame(1, 8)), Admit::Pass);
    }

    #[test]
    fn admit_passes_allowed_under_caps() {
        let mut guard = SessionGuard::new(ALLOWED, 1_024, None);
        assert_eq!(guard.admit(&frame(1, 16)), Admit::Pass);
        assert_eq!(guard.admit(&frame(2, 16)), Admit::Pass);
    }

    #[test]
    fn oversize_only_admits_all_u8_types_under_cap() {
        let mut guard = SessionGuard::oversize_only(1_024);
        // Every single-byte discriminator is admitted, including ones no
        // service knows about, so decode stays the arbiter of type validity.
        assert_eq!(guard.admit(&frame(0, 0)), Admit::Pass);
        assert_eq!(guard.admit(&frame(99, 16)), Admit::Pass);
        assert_eq!(guard.admit(&frame(255, 16)), Admit::Pass);
    }

    #[test]
    fn oversize_only_rejects_non_u8_type() {
        let mut guard = SessionGuard::oversize_only(1_024);
        assert_eq!(guard.admit(&frame(0x0100, 0)), Admit::Reject("bad type"));
    }

    #[test]
    fn oversize_only_rejects_oversize() {
        let mut guard = SessionGuard::oversize_only(4);
        assert_eq!(guard.admit(&frame(1, 5)), Admit::Reject("oversize"));
    }
}
