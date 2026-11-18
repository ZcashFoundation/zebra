//! Public listener discovery from independent, successful outbound peers.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use tokio::time::{Duration, Instant};

#[cfg(test)]
mod tests;

const MAX_OBSERVATIONS: usize = 64;
const LIFETIME: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy)]
struct Observation {
    group: IpAddr,
    address: IpAddr,
    seen: Instant,
}

/// One vote per IPv4 /16 or IPv6 /32 reporter group.
pub(super) struct ExternalAddrs {
    observations: [Option<Observation>; MAX_OBSERVATIONS],
    // Keep the last quorum after votes expire; stable connections need no new handshakes.
    elected: [Option<IpAddr>; 2],
}

impl Default for ExternalAddrs {
    fn default() -> Self {
        Self {
            observations: [None; MAX_OBSERVATIONS],
            elected: [None; 2],
        }
    }
}

impl ExternalAddrs {
    fn expire(&mut self, now: Instant) {
        for observation in &mut self.observations {
            if observation.is_some_and(|observation| now - observation.seen >= LIFETIME) {
                *observation = None;
            }
        }
    }

    pub(super) fn observe(&mut self, reporter: IpAddr, address: IpAddr, now: Instant) {
        let reporter = reporter.to_canonical();
        let address = address.to_canonical();
        // A direct connection can only reveal our address in its own canonical IP family.
        if reporter.is_ipv4() != address.is_ipv4() || !is_public(reporter) || !is_public(address) {
            return;
        }
        self.expire(now);
        let group = match reporter {
            IpAddr::V4(ip) => {
                let [a, b, _, _] = ip.octets();
                Ipv4Addr::new(a, b, 0, 0).into()
            }
            IpAddr::V6(ip) => {
                let [a, b, ..] = ip.segments();
                Ipv6Addr::new(a, b, 0, 0, 0, 0, 0, 0).into()
            }
        };
        let slot = self
            .observations
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.group == group))
            .or_else(|| self.observations.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                self.observations
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.map(|entry| entry.seen))
                    .expect("the observation table has a nonzero fixed capacity")
                    .0
            });
        self.observations[slot] = Some(Observation {
            group,
            address,
            seen: now,
        });
    }

    fn selected(&self, ipv4: bool) -> Option<IpAddr> {
        let mut selected = None;
        let mut votes = 1;
        let mut tied = false;
        // ponytail: quadratic voting is bounded to 64 entries; index candidates if that cap grows.
        for observation in self
            .observations
            .iter()
            .flatten()
            .filter(|o| o.address.is_ipv4() == ipv4)
        {
            let count = self
                .observations
                .iter()
                .flatten()
                .filter(|o| o.address == observation.address)
                .count();
            if count > votes {
                selected = Some(observation.address);
                votes = count;
                tied = false;
            } else if count == votes && selected != Some(observation.address) {
                tied = true;
            }
        }
        if tied {
            None
        } else {
            selected
        }
    }

    /// Explicit configuration wins; wildcard IPv6 listeners can advertise both families.
    pub(super) fn addresses(
        &mut self,
        listener: SocketAddr,
        external: Option<SocketAddr>,
        now: Instant,
    ) -> [Option<SocketAddr>; 2] {
        self.expire(now);
        if let Some(address) = external.or_else(|| is_public(listener.ip()).then_some(listener)) {
            return [Some(address), None];
        }
        [0, 1].map(|index| {
            let ipv4 = index == 0;
            let supported = listener.is_ipv4() == ipv4
                || (listener.is_ipv6() && listener.ip().is_unspecified());
            if !supported || listener.port() == 0 {
                return None;
            }
            self.elected[index] = self.selected(ipv4).or(self.elected[index]);
            self.elected[index].map(|ip| SocketAddr::new(ip, listener.port()))
        })
    }
}

/// Conservatively accept global unicast only, excluding special-use and transition ranges.
pub(super) fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let [a, b, ..] = ip.segments();
            a & 0xe000 == 0x2000
                && !(a == 0x2001 && (b < 0x200 || b == 0xdb8))
                && a != 0x2002
                && !(a == 0x3fff && b & 0xf000 == 0)
        }
    }
}
