//! A set of IPs from recent connection attempts.

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    time::{Duration, Instant},
};

use crate::constants;

#[derive(Debug)]
/// Stores IPs of recently attempted inbound connections.
pub struct RecentByIp {
    /// The list of IPs in increasing connection time order.
    pub by_time: VecDeque<(IpAddr, Instant)>,

    /// Stores IPs for recently attempted inbound connections.
    pub by_ip: HashMap<IpAddr, usize>,

    /// The maximum number of peer connections Zebra will keep for a given IP address
    /// before it drops any additional peer connections with that IP.
    pub max_connections_per_ip: usize,

    /// The duration to wait after an entry is added before removing it.
    pub time_limit: Duration,
}

impl Default for RecentByIp {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl RecentByIp {
    pub fn new(time_limit: Option<Duration>, max_connections_per_ip: Option<usize>) -> Self {
        let (by_time, by_ip) = Default::default();
        Self {
            by_time,
            by_ip,
            time_limit: time_limit.unwrap_or(constants::MIN_PEER_RECONNECTION_DELAY),
            max_connections_per_ip: max_connections_per_ip
                .unwrap_or(constants::DEFAULT_MAX_CONNS_PER_IP),
        }
    }

    /// Prunes outdated entries, checks if there's a recently attempted inbound connection with
    /// this IP, and adds the entry to `by_time` if it wasn't already in `by_ip`.
    ///
    /// Returns true if there was a recently attempted inbound connection.
    pub fn is_past_limit_or_add(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        self.prune_by_time(now);

        let count = self.by_ip.entry(ip).or_default();
        if *count >= self.max_connections_per_ip {
            true
        } else {
            *count += 1;
            self.by_time.push_back((ip, now));
            false
        }
    }

    /// Prunes entries older than [`constants::MIN_PEER_RECONNECTION_DELAY`].
    fn prune_by_time(&mut self, now: Instant) {
        while let Some((ip, time)) = self.by_time.pop_front() {
            if now.saturating_duration_since(time) <= constants::MIN_PEER_RECONNECTION_DELAY {
                return self.by_time.push_front((ip, time));
            }

            if let Some(count) = self.by_ip.get_mut(&ip) {
                *count -= 1;
                if *count == 0 {
                    self.by_ip.remove(&ip);
                }
            }
        }
    }
}

#[test]
fn old_connection_attempts_are_pruned() {
    let _init_guard = zebra_test::init();
    let mut recent_connections = RecentByIp::default();
    let ip = "127.0.0.1".parse().expect("should parse");

    assert!(
        !recent_connections.is_past_limit_or_add(ip),
        "should not be past limit"
    );
    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should be past max_connections_per_ip limit"
    );

    std::thread::sleep(constants::MIN_PEER_RECONNECTION_DELAY / 3);

    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should still contain entry after a third of the reconnection delay"
    );

    std::thread::sleep(3 * constants::MIN_PEER_RECONNECTION_DELAY / 4);

    assert!(
        !recent_connections.is_past_limit_or_add(ip),
        "should prune entry after 13/12 * MIN_PEER_RECONNECTION_DELAY"
    );

    const TEST_MAX_CONNS_PER_IP: usize = 3;
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);

    let mut recent_connections =
        RecentByIp::new(Some(TEST_TIME_LIMIT), Some(TEST_MAX_CONNS_PER_IP));

    for _ in 0..TEST_MAX_CONNS_PER_IP {
        assert!(
            !recent_connections.is_past_limit_or_add(ip),
            "should not be past limit"
        );
    }

    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should be past max_connections_per_ip limit"
    );

    std::thread::sleep(TEST_TIME_LIMIT / 3);

    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should still be past limit after a third of the reconnection delay"
    );

    std::thread::sleep(3 * TEST_TIME_LIMIT / 4);

    assert!(
        !recent_connections.is_past_limit_or_add(ip),
        "should prune entry after 13/12 * time_limit"
    );
}
