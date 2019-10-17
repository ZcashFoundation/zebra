//! The addressbook manages information about what peers exist, when they were
//! seen, and what services they provide.

use std::{
    collections::{BTreeMap, HashMap},
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
    by_time: BTreeMap<DateTime<Utc>, (SocketAddr, PeerServices)>,
}

impl AddressBook {
    /// Update the address book with `event`, a [`MetaAddr`] representing
    /// observation of a peer.
    pub fn update(&mut self, event: MetaAddr) {
        use chrono::Duration as CD;
        use std::collections::hash_map::Entry;

        trace!(
            ?event,
            data.total = self.by_time.len(),
            // This would be cleaner if it used "variables" but keeping
            // it inside the trace! invocation prevents running the range
            // query unless the output will actually be used.
            data.recent = self
                .by_time
                .range(
                    (Utc::now() - CD::from_std(constants::LIVE_PEER_DURATION).unwrap())..Utc::now()
                )
                .count()
        );

        let MetaAddr {
            addr,
            services,
            last_seen,
        } = event;

        match self.by_addr.entry(addr) {
            Entry::Occupied(mut entry) => {
                let (prev_last_seen, _) = entry.get();
                // If the new timestamp event is older than the current
                // one, discard it.  This is irrelevant for the timestamp
                // collector but is important for combining address
                // information from different peers.
                if *prev_last_seen > last_seen {
                    return;
                }
                self.by_time
                    .remove(prev_last_seen)
                    .expect("cannot have by_addr entry without by_time entry");
                entry.insert((last_seen, services));
                self.by_time.insert(last_seen, (addr, services));
            }
            Entry::Vacant(entry) => {
                entry.insert((last_seen, services));
                self.by_time.insert(last_seen, (addr, services));
            }
        }
    }
}
