//! Network-level metrics for Zebra.
//!
//! This module centralizes Prometheus metrics used by `zebra-network`.
//! Keeping metric definitions in one place avoids duplication and
//! makes metrics easier to audit and evolve.

/// Number of currently connected peers.
// Network-level metrics for Zebra.
/// Number of currently connected peers.
pub fn connected_peers(count: usize) {
    metrics::gauge!("zebra.network.connected_peers").set(count as f64);
}

/// Total number of peer connection attempts.
pub fn peer_connection_attempt() {
    metrics::counter!("zebra.network.peer.connection_attempts").increment(1);
}

/// Total number of successful peer connections.
pub fn peer_connection_success() {
    metrics::counter!("zebra.network.peer.connection_successes").increment(1);
}

/// Total number of failed peer connections.
pub fn peer_connection_failure() {
    metrics::counter!("zebra.network.peer.connection_failures").increment(1);
}

/// Number of inbound peer connections.
pub fn inbound_peers(count: usize) {
    metrics::gauge!("zebra.network.peers.inbound").set(count as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_functions_are_callable() {
        connected_peers(5);
        inbound_peers(3);

        peer_connection_attempt();
        peer_connection_success();
        peer_connection_failure();
    }
}
