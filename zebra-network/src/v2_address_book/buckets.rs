//! Secret-keyed bucketing for gossiped, never-attempted addresses.
//!
//! Relayed addresses are unauthenticated, attacker-suppliable data; an
//! address book that one source can fill leads to eclipse. Gossiped entries
//! are therefore admitted through fixed-size buckets chosen by a keyed hash
//! of the address's *group* and the *group of the peer that relayed it* — an
//! IPv4 /16 or IPv6 /32 prefix — in the manner of the Bitcoin Core address
//! manager's "new" table (draft, "Address Book"):
//!
//! - the gossiped population is bounded by the total bucket capacity,
//! - one source group reaches at most [`V2_SOURCE_GROUP_SPAN`] buckets, so a
//!   single relaying network range can never occupy more than that share of
//!   the book, whatever addresses it relays, and
//! - bucket positions and replacement victims are derived from a local
//!   secret, so peers cannot predict or force collisions.
//!
//! Addresses that are attempted or respond leave the gossiped state, and
//! are never evicted by gossip.

use std::net::IpAddr;

use indexmap::IndexSet;
use sha2::{Digest, Sha256};

use crate::PeerSocketAddr;

#[cfg(test)]
mod tests;

/// The number of buckets for gossiped, never-attempted addresses.
pub const V2_GOSSIP_BUCKETS: usize = 256;

/// The capacity of each gossiped-address bucket.
pub const V2_GOSSIP_BUCKET_SIZE: usize = 8;

/// The number of buckets the addresses relayed by one source group can reach.
pub const V2_SOURCE_GROUP_SPAN: usize = 16;

/// The total number of gossiped addresses the buckets track.
pub const V2_GOSSIP_CAPACITY: usize = V2_GOSSIP_BUCKETS * V2_GOSSIP_BUCKET_SIZE;

/// An address group: the IPv4 /16 or IPv6 /32 prefix of an address.
///
/// Addresses are canonical, so an IPv4-mapped IPv6 address is in the IPv4 group of its IPv4
/// address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Group {
    /// An IPv4 /16 prefix.
    V4([u8; 2]),

    /// An IPv6 /32 prefix.
    V6([u8; 4]),
}

impl Group {
    /// Returns the group of `ip`.
    pub(crate) fn of(ip: IpAddr) -> Group {
        match ip {
            IpAddr::V4(ip) => {
                let [a, b, _, _] = ip.octets();
                Group::V4([a, b])
            }
            IpAddr::V6(ip) => {
                let [a, b, c, d, ..] = ip.octets();
                Group::V6([a, b, c, d])
            }
        }
    }

    /// Feeds the group into `hasher`, with a family tag, so groups of different families never
    /// hash alike.
    fn hash_into(&self, hasher: &mut Sha256) {
        match self {
            Group::V4(prefix) => {
                hasher.update([4]);
                hasher.update(prefix);
            }
            Group::V6(prefix) => {
                hasher.update([6]);
                hasher.update(prefix);
            }
        }
    }
}

/// The secret-keyed buckets tracking gossiped, never-attempted addresses.
#[derive(Debug)]
pub(crate) struct GossipBuckets {
    /// The local secret keying bucket placement and victim choice.
    secret: [u8; 32],

    /// The bucket contents, tracking the book's gossiped entries.
    buckets: Vec<IndexSet<PeerSocketAddr>>,
}

impl GossipBuckets {
    /// Creates empty buckets with a random local secret.
    pub(crate) fn new() -> GossipBuckets {
        GossipBuckets {
            secret: rand::random(),
            buckets: vec![IndexSet::new(); V2_GOSSIP_BUCKETS],
        }
    }

    /// Creates empty buckets keyed by `secret`.
    #[cfg(test)]
    pub(crate) fn with_secret(secret: [u8; 32]) -> GossipBuckets {
        GossipBuckets {
            secret,
            buckets: vec![IndexSet::new(); V2_GOSSIP_BUCKETS],
        }
    }

    /// Admits a gossiped address, relayed by `source`, into its bucket,
    /// returning an existing entry that must be evicted from the book to
    /// make room, if the bucket is full.
    ///
    /// `is_still_gossiped` reports whether a tracked address is still a
    /// gossiped, never-attempted entry of the book: entries that were
    /// attempted, responded, or left the book are pruned from the bucket
    /// before it is considered full, and are never chosen as victims.
    #[allow(clippy::unwrap_in_result)]
    pub(crate) fn admit(
        &mut self,
        addr: PeerSocketAddr,
        source: PeerSocketAddr,
        mut is_still_gossiped: impl FnMut(PeerSocketAddr) -> bool,
    ) -> Option<PeerSocketAddr> {
        let index = self.bucket_index(&addr, &source);
        let victim_hash = self.entry_hash(&addr);
        let bucket = &mut self.buckets[index];

        if bucket.contains(&addr) {
            return None;
        }
        if bucket.len() < V2_GOSSIP_BUCKET_SIZE {
            bucket.insert(addr);
            return None;
        }

        // The bucket looks full. Heal it first: the book evicts and
        // promotes entries on its own schedule, so drop tracked addresses
        // that are no longer gossiped-only. Staleness only matters here, so
        // admissions into a bucket with room never pay for the scan.
        bucket.retain(|entry| is_still_gossiped(*entry));
        if bucket.len() < V2_GOSSIP_BUCKET_SIZE {
            bucket.insert(addr);
            return None;
        }

        // The bucket is full: replace a secret-derived victim, so a peer
        // flooding one group cannot choose which entries survive.
        //
        // The modulo is unbiased enough: the bucket length is far below
        // 2^64.
        let victim_index = (victim_hash % bucket.len() as u64) as usize;
        let victim = *bucket
            .get_index(victim_index)
            .expect("victim index is below the bucket length");
        bucket.swap_remove(&victim);
        bucket.insert(addr);

        Some(victim)
    }

    /// Removes every tracked address matching `predicate`, after a ban.
    pub(crate) fn remove_if(&mut self, predicate: impl Fn(PeerSocketAddr) -> bool) {
        for bucket in &mut self.buckets {
            bucket.retain(|entry| !predicate(*entry));
        }
    }

    /// Stops tracking `addr`, which left the gossiped state (it was tried), wherever it is.
    ///
    /// The source that relayed `addr` is not kept, so every bucket is checked; there are only
    /// [`V2_GOSSIP_CAPACITY`] entries.
    pub(crate) fn remove(&mut self, addr: PeerSocketAddr) {
        for bucket in &mut self.buckets {
            if bucket.swap_remove(&addr) {
                return;
            }
        }
    }

    /// Returns the number of tracked addresses, including any that left the gossiped state and
    /// were not pruned yet.
    pub(crate) fn len(&self) -> usize {
        self.buckets.iter().map(IndexSet::len).sum()
    }

    /// Returns the bucket index of `addr`, relayed by `source`: a keyed hash of the source
    /// group and one of [`V2_SOURCE_GROUP_SPAN`] slots, chosen by a keyed hash of both groups.
    ///
    /// So the addresses one source group relays reach at most [`V2_SOURCE_GROUP_SPAN`] buckets,
    /// and the addresses of one group relayed by one source group share a bucket.
    fn bucket_index(&self, addr: &PeerSocketAddr, source: &PeerSocketAddr) -> usize {
        let addr_group = Group::of(addr.ip());
        let source_group = Group::of(source.ip());

        let mut hasher = Sha256::new();
        hasher.update(self.secret);
        hasher.update([0x00]);
        addr_group.hash_into(&mut hasher);
        source_group.hash_into(&mut hasher);
        // The cast is safe: the span is a small constant.
        let slot = digest_value(hasher) % V2_SOURCE_GROUP_SPAN as u64;

        let mut hasher = Sha256::new();
        hasher.update(self.secret);
        hasher.update([0x01]);
        source_group.hash_into(&mut hasher);
        hasher.update(slot.to_le_bytes());

        // The casts are safe: the modulus is the small bucket count, so the result fits in a
        // `usize`.
        (digest_value(hasher) % V2_GOSSIP_BUCKETS as u64) as usize
    }

    /// Returns the keyed hash of `addr`, used to choose replacement
    /// victims.
    fn entry_hash(&self, addr: &PeerSocketAddr) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(self.secret);
        hasher.update([0x02]);
        match addr.ip() {
            IpAddr::V4(ip) => hasher.update(ip.octets()),
            IpAddr::V6(ip) => hasher.update(ip.octets()),
        }
        hasher.update(addr.port().to_le_bytes());
        digest_value(hasher)
    }
}

/// Returns the first 8 bytes of `hasher`'s digest as a number.
fn digest_value(hasher: Sha256) -> u64 {
    let digest = hasher.finalize();
    u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("a SHA-256 digest is 32 bytes, so it has 8 bytes"),
    )
}
