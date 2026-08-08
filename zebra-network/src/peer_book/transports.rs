//! Which transports each peer address is reachable on.
//!
//! Zebra speaks two peer protocols: the legacy protocol over TCP, and the
//! version 2 protocol over QUIC. Relayed addresses carry no indication of
//! which transports a peer accepts — the draft ZIP has no transport hint,
//! and the QUIC transport deliberately uses the UDP port of the same address
//! — so reachability is *learned*, never trusted:
//!
//! - a completed handshake proves the transport it used,
//! - a refused dial is remembered for a while, so the crawler stops
//!   retrying a transport that a peer does not accept, and
//! - gossip contributes nothing.
//!
//! Learned reachability is what lets a version 2 node dial version 2 peers
//! it discovered over the legacy network, instead of only the peers in its
//! configuration.
//!
//! # Interim design
//!
//! This is a side table beside the address book, not a field on
//! [`MetaAddr`](crate::meta_addr::MetaAddr), so reachability is learned
//! only from outbound dials, is not persisted across restarts, and cannot
//! influence candidate selection. Moving it onto the address record — as
//! `book/src/dev/dual-protocol-networking.md` specifies — is the next step,
//! and is what inbound handshakes, the disk peer cache, and Tor addressing
//! all need.

use std::time::{Duration, Instant};

use bitflags::bitflags;
use indexmap::IndexMap;

use crate::PeerSocketAddr;

#[cfg(test)]
mod tests;

bitflags! {
    /// The peer transports an address is known to accept.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct AddrTransports: u8 {
        /// The legacy protocol over TCP.
        const TCP = 0b0000_0001;

        /// The version 2 protocol over QUIC.
        const QUIC = 0b0000_0010;
    }
}

/// How long a refused transport is remembered before it is retried.
///
/// Peers upgrade, so a refusal must expire; until it does, the crawler
/// spends its dials on transports the peer actually accepts.
const TRANSPORT_FAILURE_RETRY_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// The maximum number of addresses with learned transport reachability.
///
/// Bounded like the address book itself: entries are attacker-suppliable
/// addresses, and the oldest are dropped when the table is full.
const MAX_TRANSPORT_ENTRIES: usize = crate::constants::MAX_ADDRS_IN_ADDRESS_BOOK;

/// What is known about one address's transport reachability.
#[derive(Copy, Clone, Debug, Default)]
struct TransportRecord {
    /// The transports that completed a handshake, or were configured.
    known: AddrTransports,

    /// The transports whose most recent dial was refused, and when.
    ///
    /// The two are one fact, so they are stored as one: a refusal always
    /// has a time, and clearing it clears both.
    refused: Option<(AddrTransports, Instant)>,
}

/// The learned transport reachability of peer addresses, in insertion
/// order so the oldest can be evicted.
#[derive(Debug, Default)]
pub(crate) struct TransportTable {
    entries: IndexMap<PeerSocketAddr, TransportRecord>,
}

impl TransportTable {
    /// Records that `addr` completed a handshake over `transport`, or is
    /// configured to be reached over it.
    pub(crate) fn record_reachable(&mut self, addr: PeerSocketAddr, transport: AddrTransports) {
        let record = self.entry(addr);
        record.known |= transport;
        record.refused = record
            .refused
            .map(|(refused, at)| (refused - transport, at))
            .filter(|(refused, _at)| !refused.is_empty());
    }

    /// Records that a dial to `addr` over `transport` was refused at `now`.
    ///
    /// A transport that previously completed a handshake stays known: a
    /// single refusal is more likely to be a restart than a downgrade, and
    /// the failure is retried after [`TRANSPORT_FAILURE_RETRY_INTERVAL`].
    pub(crate) fn record_unreachable(
        &mut self,
        addr: PeerSocketAddr,
        transport: AddrTransports,
        now: Instant,
    ) {
        let record = self.entry(addr);
        let refused = record
            .refused
            .map_or(transport, |(refused, _at)| refused | transport);
        record.refused = Some((refused, now));
    }

    /// Returns the transports `addr` is worth dialing at `now`: those it is
    /// known to accept, minus those it recently refused.
    pub(crate) fn dialable(&self, addr: &PeerSocketAddr, now: Instant) -> AddrTransports {
        let Some(record) = self.entries.get(addr) else {
            return AddrTransports::empty();
        };

        match record.refused {
            Some((refused, at))
                if now.saturating_duration_since(at) < TRANSPORT_FAILURE_RETRY_INTERVAL =>
            {
                record.known - refused
            }
            _ => record.known,
        }
    }

    /// Removes every entry matching `predicate`, after a ban.
    pub(crate) fn remove_if(&mut self, predicate: impl Fn(PeerSocketAddr) -> bool) {
        self.entries.retain(|addr, _record| !predicate(*addr));
    }

    /// Returns `addr`'s record, bounding the table first.
    fn entry(&mut self, addr: PeerSocketAddr) -> &mut TransportRecord {
        while self.entries.len() >= MAX_TRANSPORT_ENTRIES && !self.entries.contains_key(&addr) {
            // Dropping the oldest entry only costs a re-probe, and the bound
            // must hold against gossiped addresses.
            self.entries.shift_remove_index(0);
        }

        self.entries.entry(addr).or_default()
    }
}
