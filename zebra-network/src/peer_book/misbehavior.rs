//! The peer book actor's misbehavior score and ban store.
//!
//! Scores are keyed by network address and persist across connections for
//! about the ban duration: several protocol violations are detected only
//! after a delay, so a purely per-connection score would let a peer shed
//! its score - or escape a deferred penalty entirely - by reconnecting.
//! Keeping the store separate from the address book entries means book
//! churn or eviction can never launder a score.

use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use indexmap::IndexMap;

use crate::constants::{DEFAULT_BAN_DURATION, MAX_BANNED_IPS, MAX_PEER_MISBEHAVIOR_SCORE};

/// The key that misbehavior scores and bans are tracked by.
///
/// A single IPv6 operator typically controls at least a /64, so per-address
/// IPv6 bans are nearly free to evade: IPv6 scores and bans are keyed by
/// /64 prefix. A single IPv4 address behind carrier-grade NAT may be shared
/// by many independent users, so IPv4 keys are never widened - ban
/// durations are bounded instead.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BanKey {
    /// An IPv4 address.
    Ipv4(std::net::Ipv4Addr),

    /// The upper 64 bits of an IPv6 address: its /64 prefix.
    Ipv6Prefix([u8; 8]),
}

impl From<IpAddr> for BanKey {
    fn from(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(ip) => BanKey::Ipv4(ip),
            IpAddr::V6(ip) => {
                // IPv4-mapped addresses are canonicalized at the network
                // boundaries, but map them here too, so a mapped address can
                // never evade its IPv4 ban key.
                if let Some(ip) = ip.to_ipv4_mapped() {
                    BanKey::Ipv4(ip)
                } else {
                    let mut prefix = [0u8; 8];
                    prefix.copy_from_slice(&ip.octets()[..8]);
                    BanKey::Ipv6Prefix(prefix)
                }
            }
        }
    }
}

/// A peer's accumulated misbehavior score.
#[derive(Copy, Clone, Debug)]
struct ScoreEntry {
    /// The accumulated score.
    score: u32,

    /// When the last penalty was applied, for retention.
    last_penalty: Instant,
}

/// The actor's misbehavior score and ban store.
#[derive(Debug, Default)]
pub(crate) struct MisbehaviorStore {
    /// Accumulated misbehavior scores by ban key, in insertion order.
    scores: IndexMap<BanKey, ScoreEntry>,

    /// Banned keys and the time they were banned, in insertion order.
    bans: IndexMap<BanKey, Instant>,

    /// When expired scores and bans were last swept.
    last_prune: Option<Instant>,
}

/// The interval between sweeps of expired scores and bans.
///
/// Expiry is enforced per lookup inside the store, so sweeping reclaims
/// memory and tells the actor when the published bans snapshot went stale.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

impl MisbehaviorStore {
    /// Records `points` of misbehavior for `key` at `now`, and returns
    /// `true` if the accumulated score reached the ban threshold and `key`
    /// is now banned.
    ///
    /// Reaching the threshold resets the score: the ban itself carries the
    /// state forward.
    pub(crate) fn record(&mut self, key: BanKey, points: u32, now: Instant) -> bool {
        let entry = self.scores.entry(key).or_insert(ScoreEntry {
            score: 0,
            last_penalty: now,
        });
        entry.score = entry.score.saturating_add(points);
        entry.last_penalty = now;

        if entry.score >= MAX_PEER_MISBEHAVIOR_SCORE {
            self.scores.shift_remove(&key);
            self.ban(key, now);

            return true;
        }

        // Bound the number of tracked scores, dropping the oldest entries.
        while self.scores.len() > MAX_BANNED_IPS {
            self.scores.shift_remove_index(0);
        }

        false
    }

    /// Bans `key` at `now`.
    pub(crate) fn ban(&mut self, key: BanKey, now: Instant) {
        self.bans.insert(key, now);

        // Bound the number of banned keys, dropping the oldest bans.
        while self.bans.len() > MAX_BANNED_IPS {
            self.bans.shift_remove_index(0);
        }
    }

    /// Returns true if `key` is currently banned.
    pub(crate) fn is_banned(&self, key: &BanKey) -> bool {
        self.bans
            .get(key)
            .is_some_and(|banned_at| banned_at.elapsed() < DEFAULT_BAN_DURATION)
    }

    /// Returns a snapshot of the current bans, for the bans watch channel.
    pub(crate) fn bans_snapshot(&self) -> Arc<IndexMap<BanKey, Instant>> {
        Arc::new(self.bans.clone())
    }

    /// Prunes expired scores and bans, at most once per [`PRUNE_INTERVAL`].
    ///
    /// Returns `true` if any ban expired. Expiry is enforced per lookup
    /// inside the store, but the bans watch consumers only see the
    /// published snapshot, so the caller must republish it when this
    /// returns `true` — otherwise the peer set and inbound admission would
    /// enforce expired bans forever.
    pub(crate) fn prune_due(&mut self, now: Instant) -> bool {
        let due = self
            .last_prune
            .is_none_or(|last| now.saturating_duration_since(last) >= PRUNE_INTERVAL);
        if !due {
            return false;
        }
        self.last_prune = Some(now);

        let bans_before = self.bans.len();
        self.prune(now);
        bans_before != self.bans.len()
    }

    /// Drops scores that have not been updated for about the ban duration,
    /// and bans that have expired.
    ///
    /// Scores are retained on the order of the ban duration, so deferred
    /// penalties still land after a disconnect, but honest peers are not
    /// punished forever. Ban durations are bounded because an address (or
    /// prefix) can be shared by many independent users.
    fn prune(&mut self, now: Instant) {
        const SCORE_RETENTION: Duration = DEFAULT_BAN_DURATION;

        self.scores
            .retain(|_key, entry| now.duration_since(entry.last_penalty) < SCORE_RETENTION);
        self.bans
            .retain(|_key, banned_at| now.duration_since(*banned_at) < DEFAULT_BAN_DURATION);
    }
}
