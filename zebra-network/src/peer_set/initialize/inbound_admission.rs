//! The shared admission policy for inbound peer connections.
//!
//! Both the legacy TCP listener and the experimental v2 (QUIC) endpoint admit
//! inbound connections through these checks, so the ban semantics, connection
//! limits, and per-IP rate limits cannot drift between the transports.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use indexmap::IndexMap;
use tokio::sync::watch;
use tracing::debug;

use zebra_chain::parameters::Network;

use crate::{
    connection_metrics::{
        record_connection_attempt_started, record_inbound_connection_rejected, ConnectionDirection,
    },
    peer_set::{
        initialize::recent_by_ip::RecentByIp,
        limit::{ConnectionTracker, SharedConnectionCounter},
    },
    protocol::external::{canonical_peer_addr, canonical_socket_addr},
    PeerSocketAddr,
};

/// Canonicalizes a newly accepted inbound connection's address, records the
/// connection-attempt metric, and screens the peer against the ban list.
///
/// Returns the canonicalized peer address and its canonical IP, or `None` if
/// the peer is banned. On `None`, the "banned" rejection metric has been
/// recorded; the caller must close or refuse the connection, without
/// sleeping.
///
/// # Security
///
/// The address is canonicalized before it is used as a key: on a dual-stack
/// listener an IPv4 peer connects as IPv4-mapped IPv6, but bans, the per-IP
/// limiter, and the peer set are keyed on the canonical IPv4 address.
///
/// Bans are checked in both the raw and canonical representations, so a
/// banned peer cannot dodge its ban via another representation of the same
/// address, or by connecting over the other transport.
pub(super) fn screen_inbound_addr(
    network: &Network,
    addr: SocketAddr,
    bans_receiver: &watch::Receiver<Arc<IndexMap<crate::peer_book::BanKey, Instant>>>,
) -> Option<(PeerSocketAddr, IpAddr)> {
    let addr: PeerSocketAddr = canonical_peer_addr(addr);
    record_connection_attempt_started(network, ConnectionDirection::Inbound, addr);

    let canonical_ip = canonical_socket_addr(addr.remove_socket_addr_privacy()).ip();
    let bans = bans_receiver.borrow().clone();
    if bans.contains_key(&crate::peer_book::BanKey::from(addr.ip()))
        || bans.contains_key(&crate::peer_book::BanKey::from(canonical_ip))
    {
        debug!(?addr, "banned inbound connection attempt");
        record_inbound_connection_rejected(network, addr, "banned");
        return None;
    }

    Some((addr, canonical_ip))
}

/// Checks an inbound connection against the public inbound connection limit
/// and the per-IP rate limit, and reserves a public inbound slot if allowed.
///
/// `active_count` is the caller's snapshot of `active_inbound_connections`,
/// taken before any transport-specific slot decisions, so one snapshot backs
/// all of them. `limit` is the public inbound connection limit for this
/// transport.
///
/// Returns a tracker holding the slot, or `None` after recording the
/// "capacity_or_rate_limited" rejection metric. On `None`, the caller must
/// close or refuse the connection, then sleep
/// [`MIN_INBOUND_PEER_FAILED_CONNECTION_INTERVAL`](crate::constants::MIN_INBOUND_PEER_FAILED_CONNECTION_INTERVAL).
pub(super) fn try_reserve_public_inbound_slot(
    network: &Network,
    addr: PeerSocketAddr,
    active_count: usize,
    limit: usize,
    active_inbound_connections: &SharedConnectionCounter,
    recent_inbound_connections: &mut RecentByIp,
) -> Option<ConnectionTracker> {
    if active_count >= limit || recent_inbound_connections.is_past_limit_or_add(addr.ip()) {
        // Too many open inbound connections or pending handshakes already.
        record_inbound_connection_rejected(network, addr, "capacity_or_rate_limited");
        return None;
    }

    Some(active_inbound_connections.track_connection())
}
