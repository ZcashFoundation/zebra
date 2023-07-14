//! A set of IPs from recent connection attempts.

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    time::{Duration, Instant},
};

use crate::constants;

#[cfg(test)]
mod tests;

#[derive(Debug)]
/// Stores IPs of recently attempted inbound connections.
pub struct RecentByIp {
    /// The list of IPs in decreasing connection age order.
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
    /// Creates a new [`RecentByIp`]
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
    /// this IP, and adds the entry to `by_time`, and `by_ip` if needed.
    ///
    /// Returns true if the recently attempted inbound connection count is past the configured limit.
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

    /// Prunes entries older than `time_limit`, decrementing or removing their counts in `by_ip`.
    fn prune_by_time(&mut self, now: Instant) {
        // Currently saturates to zero:
        // <https://doc.rust-lang.org/std/time/struct.Instant.html#monotonicity>
        //
        // This discards the whole structure if the time limit is very large,
        // which is unexpected, but stops this list growing without limit.
        // After the handshake, the peer set will remove any duplicate connections over the limit.
        let age_limit = now - self.time_limit;

        // `by_time` must be sorted for this to work.
        let split_off_idx = self.by_time.partition_point(|&(_, time)| time <= age_limit);

        let updated_by_time = self.by_time.split_off(split_off_idx);

        for (ip, _) in &self.by_time {
            if let Some(count) = self.by_ip.get_mut(ip) {
                *count -= 1;
                if *count == 0 {
                    self.by_ip.remove(ip);
                }
            }
        }

        self.by_time = updated_by_time;
    }
}
