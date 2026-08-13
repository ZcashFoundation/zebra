//! A set of IPs from recent connection attempts.

use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
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

    /// Stores subnet keys for recently attempted inbound connections.
    /// Keyed on `/64` (IPv6) or `/24` (IPv4) subnet.
    pub by_subnet: HashMap<IpAddr, usize>,

    /// The maximum number of peer connections Zebra will keep for a given IP address
    /// before it drops any additional peer connections with that IP.
    pub max_connections_per_ip: usize,

    /// The maximum number of peer connections per IPv6 `/64` subnet.
    pub max_connections_per_subnet_v6: usize,

    /// The maximum number of peer connections per IPv4 `/24` subnet.
    pub max_connections_per_subnet_v4: usize,

    /// The duration to wait after an entry is added before removing it.
    pub time_limit: Duration,
}

impl Default for RecentByIp {
    fn default() -> Self {
        Self::new(None, None, None, None)
    }
}

impl RecentByIp {
    /// Creates a new [`RecentByIp`]
    pub fn new(
        time_limit: Option<Duration>,
        max_connections_per_ip: Option<usize>,
        max_connections_per_subnet_v6: Option<usize>,
        max_connections_per_subnet_v4: Option<usize>,
    ) -> Self {
        let (by_time, by_ip, by_subnet) = Default::default();
        Self {
            by_time,
            by_ip,
            by_subnet,
            time_limit: time_limit.unwrap_or(constants::MIN_PEER_RECONNECTION_DELAY),
            max_connections_per_ip: max_connections_per_ip
                .unwrap_or(constants::DEFAULT_MAX_CONNS_PER_IP),
            max_connections_per_subnet_v6: max_connections_per_subnet_v6
                .unwrap_or(constants::DEFAULT_MAX_CONNS_PER_SUBNET_V6),
            max_connections_per_subnet_v4: max_connections_per_subnet_v4
                .unwrap_or(constants::DEFAULT_MAX_CONNS_PER_SUBNET_V4),
        }
    }

    /// Returns the `/64` (IPv6) or `/24` (IPv4) subnet key for an IP address.
    fn subnet_key(ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], 0))
            }
            IpAddr::V6(v6) => {
                let o = v6.octets();
                let mut masked = [0u8; 16];
                masked[..8].copy_from_slice(&o[..8]);
                IpAddr::V6(Ipv6Addr::from(masked))
            }
        }
    }

    /// Prunes outdated entries, checks if there's a recently attempted inbound connection with
    /// this IP, and adds the entry to `by_time`, and `by_ip` if needed.
    ///
    /// Returns true if the recently attempted inbound connection count is past the configured
    /// per-IP or per-subnet limit.
    pub fn is_past_limit_or_add(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        self.prune_by_time(now);

        // # Security
        //
        // Check the per-IP limit first. Each distinct IpAddr gets its own counter,
        // so this correctly limits connections from one exact IP.
        let ip_count = self.by_ip.get(&ip).copied().unwrap_or(0);
        if ip_count >= self.max_connections_per_ip {
            return true;
        }

        // # Security
        //
        // Check the per-subnet limit. A single VPS with a standard IPv6 /64 allocation
        // has 18 quintillion distinct IpAddr values, each passing the per-IP check above
        // individually. The subnet key groups them by /64 (IPv6) or /24 (IPv4) so the
        // per-subnet cap can limit the total from one machine.
        let subnet = Self::subnet_key(ip);
        let subnet_count = self.by_subnet.get(&subnet).copied().unwrap_or(0);
        let subnet_limit = match ip {
            IpAddr::V4(_) => self.max_connections_per_subnet_v4,
            IpAddr::V6(_) => self.max_connections_per_subnet_v6,
        };
        if subnet_count >= subnet_limit {
            return true;
        }

        // Accept: increment both IP and subnet counters.
        *self.by_ip.entry(ip).or_default() += 1;
        *self.by_subnet.entry(subnet).or_default() += 1;
        self.by_time.push_back((ip, now));
        false
    }

    /// Prunes entries older than `time_limit`, decrementing or removing their counts in
    /// `by_ip` and `by_subnet`.
    fn prune_by_time(&mut self, now: Instant) {
        // Currently saturates to zero:
        // <https://doc.rust-lang.org/std/time/struct/Instant.html#monotonicity>
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

            let subnet = Self::subnet_key(*ip);
            if let Some(count) = self.by_subnet.get_mut(&subnet) {
                *count -= 1;
                if *count == 0 {
                    self.by_subnet.remove(&subnet);
                }
            }
        }

        self.by_time = updated_by_time;
    }
}
