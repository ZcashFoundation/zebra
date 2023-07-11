//! A set of IPs from recent connection attempts.

use std::{
    collections::{HashSet, VecDeque},
    net::IpAddr,
    time::Instant,
};

use crate::constants;

#[derive(Debug, Default)]
/// Stores IPs of recently attempted inbound connections.
pub struct RecentByIp {
    /// The list of IPs in increasing connection time order.
    pub by_time: VecDeque<(IpAddr, Instant)>,

    /// Stores a set of IPs for recently attempted inbound connections.
    // TODO: Replace with `HashMap<IpAddr, usize>` to support
    //       configured `max_connections_per_ip` greater than 1.
    pub by_ip: HashSet<IpAddr>,
}

impl RecentByIp {
    /// Prunes outdated entries, checks if there's a recently attempted inbound connection with
    /// this IP, and adds the entry to `by_time` if it wasn't already in `by_ip`.
    ///
    /// Returns true if there was a recently attempted inbound connection.
    #[allow(dead_code)]
    pub fn has_or_add(&mut self, ip: IpAddr) -> bool {
        self.prune_by_time();
        if self.by_ip.insert(ip) {
            self.by_time.push_back((ip, Instant::now()));
            false
        } else {
            true
        }
    }

    /// Prunes entries older than [`constants::MIN_PEER_RECONNECTION_DELAY`].
    #[allow(dead_code)]
    fn prune_by_time(&mut self) {
        let now = Instant::now();
        while let Some((ip, time)) = self.by_time.pop_front() {
            if now.saturating_duration_since(time) <= constants::MIN_PEER_RECONNECTION_DELAY {
                return self.by_time.push_front((ip, time));
            }
            self.by_ip.remove(&ip);
        }
    }
}

#[test]
fn old_connection_attempts_are_pruned() {
    let _init_guard = zebra_test::init();
    let mut recent_connections = RecentByIp::default();
    let ip = "127.0.0.1".parse().expect("should parse");

    assert!(
        !recent_connections.has_or_add(ip),
        "new RecentConnections should be empty"
    );
    assert!(recent_connections.has_or_add(ip), "should now contain ip");

    std::thread::sleep(constants::MIN_PEER_RECONNECTION_DELAY / 3);

    assert!(
        recent_connections.has_or_add(ip),
        "should still contain entry after a third of the reconnection delay"
    );

    std::thread::sleep(3 * constants::MIN_PEER_RECONNECTION_DELAY / 4);

    assert!(
        !recent_connections.has_or_add(ip),
        "should prune entry after 13/12 * MIN_PEER_RECONNECTION_DELAY"
    );
}
