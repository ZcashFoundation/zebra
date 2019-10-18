//! The addressbook manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{
    collections::{BTreeSet, HashMap},
    iter::Extend,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use futures::channel::mpsc;
use tokio::prelude::*;

use crate::{
    constants,
    types::{MetaAddr, PeerServices},
};

/// A database of peers, their advertised services, and information on when they
/// were last seen.
#[derive(Default, Debug)]
pub struct AddressBook {
    by_addr: HashMap<SocketAddr, (DateTime<Utc>, PeerServices)>,
    by_time: BTreeSet<MetaAddr>,
}

impl AddressBook {
    fn assert_consistency(&self) {
        for (a, (t, s)) in self.by_addr.iter() {
            for meta in self.by_time.iter().filter(|meta| meta.addr == *a) {
                if meta.last_seen != *t || meta.services != *s {
                    panic!("meta {:?} is not {:?}, {:?}, {:?}", meta, a, t, s);
                }
            }
        }
    }

    /// Update the address book with `event`, a [`MetaAddr`] representing
    /// observation of a peer.
    pub fn update(&mut self, event: MetaAddr) {
        use std::collections::hash_map::Entry;

        trace!(
            ?event,
            data.total = self.by_time.len(),
            data.recent = (self.by_time.len() - self.disconnected_peers().count()),
        );
        //self.assert_consistency();

        let MetaAddr {
            addr,
            services,
            last_seen,
        } = event;

        match self.by_addr.entry(addr) {
            Entry::Occupied(mut entry) => {
                let (prev_last_seen, prev_services) = entry.get().clone();
                // Ignore stale entries.
                if prev_last_seen > last_seen {
                    return;
                }
                self.by_time
                    .take(&MetaAddr {
                        addr,
                        services: prev_services,
                        last_seen: prev_last_seen,
                    })
                    .expect("cannot have by_addr entry without by_time entry");
                entry.insert((last_seen, services));
                self.by_time.insert(event);
            }
            Entry::Vacant(entry) => {
                entry.insert((last_seen, services));
                self.by_time.insert(event);
            }
        }
        //self.assert_consistency();
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

    /// Returns true if the given [`SocketAddr`] could potentially be connected
    /// to a node feeding timestamps into this address book.
    pub fn is_potentially_connected(&self, addr: &SocketAddr) -> bool {
        match self.by_addr.get(addr) {
            None => return false,
            Some((ref last_seen, _)) => last_seen > &AddressBook::cutoff_time(),
        }
    }

    /// Return an iterator over all peers, ordered from most recently seen to
    /// least recently seen.
    pub fn peers<'a>(&'a self) -> impl Iterator<Item = MetaAddr> + 'a {
        self.by_time.iter().rev().cloned()
    }

    /// Return an iterator over peers known to be disconnected, ordered from most
    /// recently seen to least recently seen.
    pub fn disconnected_peers<'a>(&'a self) -> impl Iterator<Item = MetaAddr> + 'a {
        use std::net::{IpAddr, Ipv4Addr};
        use std::ops::Bound::{Excluded, Unbounded};
        let cutoff_meta = MetaAddr {
            last_seen: AddressBook::cutoff_time(),
            // The ordering on MetaAddrs is newest-first, then arbitrary,
            // so any fields will do here.
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0),
            services: PeerServices::default(),
        };

        self.by_time
            .range((Excluded(cutoff_meta), Unbounded))
            .rev()
            .cloned()
    }

    /// Returns an iterator that drains entries from the address book, removing
    /// them in order from most recent to least recent.
    pub fn drain_newest<'a>(&'a mut self) -> impl Iterator<Item = MetaAddr> + 'a {
        Drain {
            book: self,
            newest_first: true,
        }
    }

    /// Returns an iterator that drains entries from the address book, removing
    /// them in order from most recent to least recent.
    pub fn drain_oldest<'a>(&'a mut self) -> impl Iterator<Item = MetaAddr> + 'a {
        Drain {
            book: self,
            newest_first: false,
        }
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
            self.book.by_time.iter().next()?.clone()
        } else {
            self.book.by_time.iter().rev().next()?.clone()
        };
        self.book.by_time.remove(&next_item);
        self.book
            .by_addr
            .remove(&next_item.addr)
            .expect("cannot have by_time entry without by_addr entry");
        Some(next_item)
    }
}
