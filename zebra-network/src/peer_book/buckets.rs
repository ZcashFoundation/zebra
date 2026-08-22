//! Secret-keyed bucketing for gossiped, never-attempted addresses.
//!
//! Relayed addresses are unauthenticated, attacker-suppliable data; an
//! address book that one source can fill leads to eclipse. Gossiped entries
//! are therefore admitted through fixed-size buckets chosen by a keyed hash
//! of the address's *group* — its IPv4 /16 or IPv6 /32 prefix, in the
//! manner of the Bitcoin Core address manager:
//!
//! - the gossiped population is bounded by the total bucket capacity,
//! - one address group maps to one bucket, so a single network range can
//!   never occupy more than one bucket's worth of the book, and
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
pub const GOSSIP_BUCKET_COUNT: usize = 256;

/// The capacity of each gossiped-address bucket.
pub const GOSSIP_BUCKET_SIZE: usize = 32;

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
            buckets: vec![IndexSet::new(); GOSSIP_BUCKET_COUNT],
        }
    }

    /// Admits a gossiped address into its bucket, returning an existing
    /// entry that must be evicted from the book to make room, if the
    /// bucket is full.
    ///
    /// `is_still_gossiped` reports whether a tracked address is still a
    /// gossiped, never-attempted entry of the book: entries that were
    /// attempted, responded, or left the book are pruned from the bucket
    /// before it is considered full, and are never chosen as victims.
    #[allow(clippy::unwrap_in_result)]
    pub(crate) fn admit(
        &mut self,
        addr: PeerSocketAddr,
        mut is_still_gossiped: impl FnMut(PeerSocketAddr) -> bool,
    ) -> Option<PeerSocketAddr> {
        let index = self.bucket_index(&addr);
        let victim_hash = self.entry_hash(&addr);
        let bucket = &mut self.buckets[index];

        if bucket.contains(&addr) {
            return None;
        }
        if bucket.len() < GOSSIP_BUCKET_SIZE {
            bucket.insert(addr);
            return None;
        }

        // The bucket looks full. Heal it first: the book evicts and
        // promotes entries on its own schedule, so drop tracked addresses
        // that are no longer gossiped-only. Staleness only matters here, so
        // admissions into a bucket with room never pay for the scan.
        bucket.retain(|entry| is_still_gossiped(*entry));
        if bucket.len() < GOSSIP_BUCKET_SIZE {
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

    /// Returns the bucket index of `addr`: a keyed hash of its group.
    fn bucket_index(&self, addr: &PeerSocketAddr) -> usize {
        let mut hasher = Sha256::new();
        hasher.update(self.secret);
        hasher.update([0x00]);
        match addr.ip() {
            // The group is the IPv4 /16 prefix,
            IpAddr::V4(ip) => hasher.update(&ip.octets()[..2]),
            // or the IPv6 /32 prefix.
            IpAddr::V6(ip) => hasher.update(&ip.octets()[..4]),
        }
        let digest = hasher.finalize();
        let value = u64::from_le_bytes(digest[..8].try_into().expect("digest has 8 bytes"));

        // The cast is safe: the modulus is the small bucket count.
        (value % GOSSIP_BUCKET_COUNT as u64) as usize
    }

    /// Returns the keyed hash of `addr`, used to choose replacement
    /// victims.
    fn entry_hash(&self, addr: &PeerSocketAddr) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(self.secret);
        hasher.update([0x01]);
        match addr.ip() {
            IpAddr::V4(ip) => hasher.update(ip.octets()),
            IpAddr::V6(ip) => hasher.update(ip.octets()),
        }
        hasher.update(addr.port().to_le_bytes());
        let digest = hasher.finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("digest has 8 bytes"))
    }
}
