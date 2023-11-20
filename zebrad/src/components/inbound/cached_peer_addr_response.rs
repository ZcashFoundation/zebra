//! Periodically-refreshed GetAddr response for the inbound service.
//!
//! Used to avoid giving out Zebra's entire address book over a short duration.

use std::{
    sync::{Mutex, TryLockError},
    time::Instant,
};

use super::*;

/// Caches and refreshes a partial list of peer addresses to be returned as a `GetAddr` response.
pub struct CachedPeerAddrResponse {
    /// A shared list of peer addresses.
    address_book: Arc<Mutex<zn::AddressBook>>,

    /// An owned list of peer addresses used as a `GetAddr` response.
    value: zn::Response,

    /// Instant after which `cached_addrs` should be refreshed.
    refresh_time: Instant,
}

impl CachedPeerAddrResponse {
    /// Creates a new empty [`CachedPeerAddrResponse`].
    pub(super) fn new(address_book: Arc<Mutex<AddressBook>>) -> Self {
        Self {
            address_book,
            value: zn::Response::Nil,
            refresh_time: Instant::now(),
        }
    }

    pub(super) fn value(&self) -> zn::Response {
        self.value.clone()
    }

    /// Refreshes the `cached_addrs` if the time has past `refresh_time` or the cache is empty
    pub(super) fn try_refresh(&mut self) {
        let now = Instant::now();

        // return early if there are some cached addresses, and they are still fresh
        if now < self.refresh_time {
            return;
        }

        // try getting a lock on the address book if it's time to refresh the cached addresses
        match self
            .address_book
            .try_lock()
            .map(|book| book.fresh_get_addr_response())
        {
            // update cached value and refresh_time if there _are_ peers in the address books.
            Ok(peers) if !peers.is_empty() => {
                self.refresh_time = now + INBOUND_CACHED_ADDRS_REFRESH_INTERVAL;
                self.value = zn::Response::Peers(peers);
            }

            Ok(_) => {
                debug!(
                    "could not refresh cached response because our address \
                     book has no available peers"
                );
            }

            Err(TryLockError::WouldBlock) => {
                let next_refresh_time = self.refresh_time + INBOUND_CACHED_ADDRS_REFRESH_INTERVAL;

                if now > next_refresh_time {
                    warn!("getaddrs response hasn't been refreshed in some time");
                };
            }
            Err(TryLockError::Poisoned(_)) => {
                panic!("previous thread panicked while holding the address book lock")
            }
        };
    }
}
