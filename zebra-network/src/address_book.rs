//! The addressbook manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{
    collections::{BTreeSet, HashMap},
    iter::Extend,
    net::SocketAddr,
};

use chrono::{DateTime, Utc};
use tracing::Span;

use crate::{
    constants,
    types::{MetaAddr, PeerServices},
};

/// A database of peers, their advertised services, and information on when they
/// were last seen.
#[derive(Debug)]
pub struct AddressBook {
    by_addr: HashMap<SocketAddr, (DateTime<Utc>, PeerServices)>,
    by_time: BTreeSet<MetaAddr>,
    span: Span,
}

#[allow(clippy::len_without_is_empty)]
impl AddressBook {
    /// Construct an `AddressBook` with the given [`tracing::Span`].
    pub fn new(span: Span) -> AddressBook {
        AddressBook {
            by_addr: HashMap::default(),
            by_time: BTreeSet::default(),
            span,
        }
    }

    /// Get the contents of `self` in random order with sanitized timestamps.
    pub fn sanitized(&self) -> Vec<MetaAddr> {
        use rand::seq::SliceRandom;
        let mut peers = self.peers().map(MetaAddr::sanitize).collect::<Vec<_>>();
        peers.shuffle(&mut rand::thread_rng());
        peers
    }

    /// Check consistency of the address book invariants or panic, doing work
    /// quadratic in the address book size.
    #[cfg(test)]
    fn assert_consistency(&self) {
        for (a, (t, s)) in self.by_addr.iter() {
            for meta in self.by_time.iter().filter(|meta| meta.addr == *a) {
                if meta.last_seen != *t || meta.services != *s {
                    panic!("meta {:?} is not {:?}, {:?}, {:?}", meta, a, t, s);
                }
            }
        }
    }

    /// Returns true if the address book has an entry for `addr`.
    pub fn contains_addr(&self, addr: &SocketAddr) -> bool {
        let _guard = self.span.enter();
        self.by_addr.contains_key(addr)
    }

    /// Returns the entry corresponding to `addr`, or `None` if it does not exist.
    pub fn get_by_addr(&self, addr: SocketAddr) -> Option<MetaAddr> {
        let _guard = self.span.enter();
        let (last_seen, services) = self.by_addr.get(&addr).cloned()?;
        Some(MetaAddr {
            addr,
            last_seen,
            services,
        })
    }

    /// Add `new` to the address book, updating the previous entry if `new` is
    /// more recent or discarding `new` if it is stale.
    pub fn update(&mut self, new: MetaAddr) {
        let _guard = self.span.enter();
        trace!(
            ?new,
            data.total = self.by_time.len(),
            data.recent = (self.by_time.len() - self.disconnected_peers().count()),
        );
        #[cfg(test)]
        self.assert_consistency();

        if let Some(prev) = self.get_by_addr(new.addr) {
            if prev.last_seen > new.last_seen {
                return;
            } else {
                self.by_time
                    .take(&prev)
                    .expect("cannot have by_addr entry without by_time entry");
            }
        }
        self.by_time.insert(new);
        self.by_addr.insert(new.addr, (new.last_seen, new.services));

        #[cfg(test)]
        self.assert_consistency();
    }

    /// Compute a cutoff time that can determine whether an entry
    /// in an address book being updated with peer message timestamps
    /// represents a known-disconnected peer or a potentially-connected peer.
    ///
    /// [`constants::LIVE_PEER_DURATION`] represents the time interval in which
    /// we are guaranteed to receive at least one message from a peer or close
    /// the connection. Therefore, if the last-seen timestamp is older than
    /// [`constants::LIVE_PEER_DURATION`] ago, we know we must have disconnected
    /// from it. Otherwise, we could potentially be connected to it.
    fn cutoff_time() -> DateTime<Utc> {
        // chrono uses signed durations while stdlib uses unsigned durations
        use chrono::Duration as CD;
        Utc::now() - CD::from_std(constants::LIVE_PEER_DURATION).unwrap()
    }

    /// Used for range bounds, see cutoff_time
    fn cutoff_meta() -> MetaAddr {
        use std::net::{IpAddr, Ipv4Addr};
        MetaAddr {
            last_seen: AddressBook::cutoff_time(),
            // The ordering on MetaAddrs is newest-first, then arbitrary,
            // so any fields will do here.
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
            services: PeerServices::default(),
        }
    }

    /// Returns true if the given [`SocketAddr`] could potentially be connected
    /// to a node feeding timestamps into this address book.
    pub fn is_potentially_connected(&self, addr: &SocketAddr) -> bool {
        let _guard = self.span.enter();
        match self.by_addr.get(addr) {
            None => false,
            Some((ref last_seen, _)) => last_seen > &AddressBook::cutoff_time(),
        }
    }

    /// Return an iterator over all peers, ordered from most recently seen to
    /// least recently seen.
    pub fn peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();
        self.by_time.iter().rev().cloned()
    }

    /// Return an iterator over peers known to be disconnected, ordered from most
    /// recently seen to least recently seen.
    pub fn disconnected_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();
        use std::ops::Bound::{Excluded, Unbounded};

        self.by_time
            .range((Excluded(Self::cutoff_meta()), Unbounded))
            .rev()
            .cloned()
    }

    /// Return an iterator over peers that could potentially be connected, ordered from most
    /// recently seen to least recently seen.
    pub fn potentially_connected_peers(&'_ self) -> impl Iterator<Item = MetaAddr> + '_ {
        let _guard = self.span.enter();
        use std::ops::Bound::{Included, Unbounded};

        self.by_time
            .range((Unbounded, Included(Self::cutoff_meta())))
            .rev()
            .cloned()
    }

    /// Returns an iterator that drains entries from the address book, removing
    /// them in order from most recent to least recent.
    pub fn drain_newest(&'_ mut self) -> impl Iterator<Item = MetaAddr> + '_ {
        Drain {
            book: self,
            newest_first: true,
        }
    }

    /// Returns an iterator that drains entries from the address book, removing
    /// them in order from least recent to most recent.
    pub fn drain_oldest(&'_ mut self) -> impl Iterator<Item = MetaAddr> + '_ {
        Drain {
            book: self,
            newest_first: false,
        }
    }

    /// Returns the number of entries in this address book.
    pub fn len(&self) -> usize {
        self.by_time.len()
    }
}

impl Extend<MetaAddr> for AddressBook {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = MetaAddr>,
    {
        for meta in iter.into_iter() {
            self.update(meta);
        }
    }
}

struct Drain<'a> {
    book: &'a mut AddressBook,
    newest_first: bool,
}

impl<'a> Iterator for Drain<'a> {
    type Item = MetaAddr;

    fn next(&mut self) -> Option<Self::Item> {
        let next_item = if self.newest_first {
            *self.book.by_time.iter().next()?
        } else {
            *self.book.by_time.iter().rev().next()?
        };
        self.book.by_time.remove(&next_item);
        self.book
            .by_addr
            .remove(&next_item.addr)
            .expect("cannot have by_time entry without by_addr entry");
        Some(next_item)
    }
}
