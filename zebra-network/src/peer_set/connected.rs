//! The peer set's connected keys, shared with the legacy and version 2 dialers.
//!
//! The peer set keeps at most one connection per remote address across both protocols, and
//! drops a new connection whose address is already connected, or whose IP is at the per-IP
//! limit. The dialers check the same rules before they dial, so they open fewer connections
//! the peer set would drop.
//!
//! The check is best-effort: it only sees admitted connections. Handshakes still in progress,
//! over either protocol, reserve nothing, so two dials to one address or IP can both pass it.
//! The peer set's own check stays authoritative, and drops the later of the racing connections.

use std::{collections::HashSet, sync::Arc};

use tokio::sync::watch;

use crate::{protocol::external::connection_limit_key, PeerSocketAddr};

#[cfg(test)]
mod tests;

/// The addresses of the peer set's live connections, over either protocol, and the per-IP
/// connection limit the peer set enforces on them.
#[derive(Clone, Debug)]
pub(crate) struct ConnectedPeers {
    /// The peer set's connected keys, ready or busy.
    keys: watch::Receiver<Arc<HashSet<PeerSocketAddr>>>,

    /// The peer set's limit on connections per IPv4 address or IPv6 `/64`.
    max_conns_per_ip: usize,
}

impl ConnectedPeers {
    /// Returns the connected peers in `keys`, limited to `max_conns_per_ip` per IP.
    pub(crate) fn new(
        keys: watch::Receiver<Arc<HashSet<PeerSocketAddr>>>,
        max_conns_per_ip: usize,
    ) -> Self {
        Self {
            keys,
            max_conns_per_ip,
        }
    }

    /// Returns connected peers with no keys, which never block a dial.
    #[cfg(test)]
    pub(crate) fn unconnected() -> Self {
        let (_keys_tx, keys) = watch::channel(Arc::new(HashSet::new()));
        Self::new(keys, usize::MAX)
    }

    /// Returns why the peer set would drop a new connection to `addr`, if it would.
    ///
    /// The peer set drops a connection to an address it is already connected to, or to an
    /// IPv4 address or IPv6 `/64` that already has `max_conns_per_ip` connections.
    ///
    /// Only admitted connections count: a pending handshake to `addr`, or to its IP, doesn't
    /// block the dial, so the peer set can still drop the connection when it completes.
    pub(crate) fn blocks_dial(&self, addr: PeerSocketAddr) -> Option<&'static str> {
        let keys = self.keys.borrow();
        if keys.contains(&addr) {
            return Some("already connected");
        }

        let limit_key = connection_limit_key(addr.ip());
        let same_ip = keys
            .iter()
            .filter(|key| connection_limit_key(key.ip()) == limit_key)
            .count();
        (same_ip >= self.max_conns_per_ip).then_some("per-IP limit")
    }
}
