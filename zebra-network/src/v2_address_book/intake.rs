//! The intake rules for relayed version 2 address records (draft, "Address Records", "Address
//! Book").

use zebra_chain::serialization::{DateTime32, Duration32};

use crate::{
    meta_addr::MetaAddr,
    protocol::{external::types::PeerServices, v2::announcement::clamp_relayed_time},
    PeerAddrState, PeerSocketAddr,
};

use super::{
    actor::V2Book,
    buckets::{Admission, Incumbent},
    Via,
};

/// Relayed records whose `time`, after clamping, is older than this are dropped (draft, "Address
/// Records": SHOULD NOT relay, and SHOULD evict, such records).
pub(super) const MAX_RELAYED_RECORD_AGE: Duration32 = Duration32::from_days(30);

/// Why a relayed record was not stored as it was received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rejection {
    /// Its address cannot be dialled: unspecified, port zero, or another network's port.
    Unroutable,

    /// It is this node's own advertised address.
    OwnAddress,

    /// Its address is banned.
    Banned,

    /// Its `time` was zero or too far in the future, and was clamped. The record is kept.
    Clamped,

    /// Its `time`, after clamping, is more than [`MAX_RELAYED_RECORD_AGE`] old.
    Stale,

    /// It does not claim to serve the chain.
    NotANode,

    /// Its bucket is full of relayed entries that are not stale, so it was dropped.
    BucketFull,

    /// It took the bucket slot of a stale relayed entry, which was removed.
    BucketEvicted,
}

impl Rejection {
    /// The metric label for this rejection.
    fn label(&self) -> &'static str {
        match self {
            Rejection::Unroutable => "unroutable",
            Rejection::OwnAddress => "self",
            Rejection::Banned => "banned",
            Rejection::Clamped => "clamped",
            Rejection::Stale => "stale",
            Rejection::NotANode => "not_a_node",
            Rejection::BucketFull => "bucket_full",
            Rejection::BucketEvicted => "bucket_evicted",
        }
    }

    /// Counts this outcome. Only the records that were dropped count as rejected: a clamped
    /// record is kept, and an eviction removes an entry the segment already had to keep the
    /// incoming record, so each has its own counter.
    fn count(&self) {
        match self {
            Rejection::Clamped => metrics::counter!("zcash.net.v2.addresses.clamped").increment(1),
            Rejection::BucketEvicted => {
                metrics::counter!("zcash.net.v2.addresses.evicted").increment(1)
            }
            _ => metrics::counter!("zcash.net.v2.addresses.rejected", "reason" => self.label())
                .increment(1),
        }
    }
}

/// Returns whether the relayed `time` of `entry`, which was clamped when it was stored, is more
/// than [`MAX_RELAYED_RECORD_AGE`] before `now`.
fn is_stale(entry: &MetaAddr, now: DateTime32) -> bool {
    entry
        .untrusted_last_seen()
        .is_none_or(|time| now.saturating_duration_since(time) > MAX_RELAYED_RECORD_AGE)
}

impl V2Book {
    /// Applies the records that `source` relayed `via` a `get-addr` answer or announcements.
    pub(super) fn intake(&mut self, source: PeerSocketAddr, addrs: Vec<MetaAddr>, via: Via) {
        self.intake_at(source, addrs, via, DateTime32::now());
    }

    /// Applies relayed records at `now`, in order, and returns how each one was treated, for
    /// tests.
    pub(super) fn intake_at(
        &mut self,
        source: PeerSocketAddr,
        addrs: Vec<MetaAddr>,
        via: Via,
        now: DateTime32,
    ) -> Vec<Option<Rejection>> {
        // The cast is safe: a peer relays at most a few thousand records at once.
        metrics::counter!("zcash.net.v2.addresses.received", "via" => via.label())
            .increment(addrs.len() as u64);

        addrs
            .into_iter()
            .map(|addr| {
                let outcome = self.intake_one(source, addr, now);
                if let Some(rejection) = outcome {
                    rejection.count();
                }
                outcome
            })
            .collect()
    }

    /// Applies one relayed record. Records with unsupported network IDs never get here: they
    /// do not decode into a [`MetaAddr`].
    fn intake_one(
        &mut self,
        source: PeerSocketAddr,
        mut record: MetaAddr,
        now: DateTime32,
    ) -> Option<Rejection> {
        let addr = record.addr;

        if !record.address_is_valid_for_outbound(&self.network) {
            return Some(Rejection::Unroutable);
        }
        if addr == self.local_listener {
            return Some(Rejection::OwnAddress);
        }
        if self.bans.is_banned(addr.ip()) {
            return Some(Rejection::Banned);
        }
        // The book only stores never-attempted entries that claim to serve the chain, so such a
        // record would only take a bucket slot.
        if !record
            .services
            .is_some_and(|services| services.contains(PeerServices::NODE_NETWORK))
        {
            return Some(Rejection::NotANode);
        }

        // Decoded records always have a time.
        let time = record.untrusted_last_seen().unwrap_or(DateTime32::MIN);
        let clamped = clamp_relayed_time(time, now);
        let outcome = (clamped != time).then_some(Rejection::Clamped);
        record.set_untrusted_last_seen(clamped);
        if is_stale(&record, now) {
            return Some(Rejection::Stale);
        }

        // Never `None`: the services were checked above.
        let Some(change) = record.new_gossiped_change() else {
            return Some(Rejection::NotANode);
        };

        // An entry the segment has, in any state, is only refreshed: the book ignores a
        // never-attempted change to an attempted entry, so relay cannot reset this node's own
        // connection record.
        if self.book.get(addr).is_some() {
            self.book.update(change);
            return outcome;
        }

        // # Security
        //
        // A new relayed address is admitted through the secret-keyed buckets, by its group and
        // the relaying peer's group, so one source group can only fill its share of them.
        // Trusted and tried entries are never victims, and a relayed entry is only a victim
        // once its own record is stale. An entry with a failed attempt is tried, so it has left
        // its bucket already.
        let admission = {
            let book = &mut self.book;
            let trusted = &self.trusted;
            self.buckets.admit(addr, source, |entry| {
                let relayed = book.get(entry).filter(|meta| {
                    meta.last_connection_state == PeerAddrState::NeverAttemptedGossiped
                        && !trusted.contains_key(&entry)
                });
                match relayed {
                    None => Incumbent::Gone,
                    Some(meta) if is_stale(&meta, now) => Incumbent::Terrible,
                    Some(_) => Incumbent::Kept,
                }
            })
        };
        match admission {
            Admission::Admitted => {}
            Admission::Replaced(victim) => {
                self.book.remove(victim);
                Rejection::BucketEvicted.count();
            }
            Admission::Full => return Some(Rejection::BucketFull),
        }

        if self.book.update(change).is_none() {
            // The book refused the entry, so it holds no slot.
            self.buckets.remove(addr);
        }
        outcome
    }
}
