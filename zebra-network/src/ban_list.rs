//! The set of banned peer groups.

use std::{collections::HashMap, net::IpAddr, sync::Arc};

use tokio::time::Instant;

use crate::{constants, protocol::external::connection_limit_key};

#[cfg(test)]
mod tests;

/// The peer groups Zebra has banned for misbehaviour, and when each was banned.
///
/// Entries are keyed by peer group — one IPv4 address, or one IPv6 `/64` subnet
/// — so a peer cannot dodge its ban by reconnecting from another address it
/// already controls. Bans lapse after
/// [`BAN_DURATION`](constants::BAN_DURATION).
///
/// # Security
///
/// This type owns both of those rules. It deliberately does not expose the
/// underlying map: a caller doing its own lookup would have to remember to map
/// the address to its peer group *and* to check the ban's age, and getting
/// either wrong silently stops bans being enforced. Query it with
/// [`BanList::is_banned`].
///
/// # Correctness
///
/// Cloning is cheap, and a clone is a snapshot: the map is shared behind an
/// [`Arc`] and copied only when a ban is added. Snapshots stay correct as bans
/// lapse, because [`BanList::is_banned`] checks each entry's age when it is
/// queried, so holders don't need to be sent a new snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BanList {
    /// The time each banned peer group was banned.
    banned_at: Arc<HashMap<IpAddr, Instant>>,
}

impl BanList {
    /// Returns `true` if `ip`'s peer group is banned, and the ban has not
    /// lapsed.
    pub fn is_banned(&self, ip: IpAddr) -> bool {
        self.banned_at
            .get(&connection_limit_key(ip))
            .is_some_and(|banned_at| !Self::has_lapsed(*banned_at, Instant::now()))
    }

    /// Bans `ip`'s peer group, starting now.
    ///
    /// Re-banning an already-banned group extends its ban for another full
    /// [`BAN_DURATION`](constants::BAN_DURATION).
    pub(crate) fn ban(&mut self, ip: IpAddr) {
        let now = Instant::now();
        let banned_at = Arc::make_mut(&mut self.banned_at);

        // Drop lapsed bans, so they don't occupy the slots that active bans
        // need, and so snapshots stay small.
        banned_at.retain(|_group, entry| !Self::has_lapsed(*entry, now));

        // Inserting an already-banned group overwrites its ban time, which is
        // exactly the refresh we want.
        banned_at.insert(connection_limit_key(ip), now);

        while banned_at.len() > constants::MAX_BANNED_IPS {
            let oldest = banned_at
                .iter()
                .min_by_key(|(_group, entry)| **entry)
                .map(|(group, _entry)| *group)
                .expect("the map is over the limit, so it is not empty");
            banned_at.remove(&oldest);
        }
    }

    /// Returns `true` if a ban applied at `banned_at` has lapsed by `now`.
    fn has_lapsed(banned_at: Instant, now: Instant) -> bool {
        // Instants are monotonic, so `now` is normally at or after `banned_at`.
        // Saturating to zero treats a clock oddity as "just banned" rather than
        // "lapsed", which fails closed.
        now.saturating_duration_since(banned_at) >= constants::BAN_DURATION
    }

    /// Returns the number of banned peer groups, including any whose bans have
    /// lapsed but have not been pruned yet.
    pub fn len(&self) -> usize {
        self.banned_at.len()
    }

    /// Returns `true` if no peer group is banned.
    pub fn is_empty(&self) -> bool {
        self.banned_at.is_empty()
    }
}
