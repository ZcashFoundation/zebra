//! Periodically-refreshed GetAddr response for the inbound service.
//!
//! Used to avoid giving out Zebra's entire address book over a short duration.

use std::{
    sync::{Arc, Mutex, TryLockError},
    time::Instant,
};

use zebra_network::{AddressBook, AddressBookService};

use super::*;

/// The minimum duration that a `CachedPeerAddrResponse` is considered fresh before the inbound service
/// should get new peer addresses from the address book to send as a `GetAddr` response.
///
/// Cached responses are considered stale and should be cleared after twice this duration.
pub const CACHED_ADDRS_REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Caches and refreshes a partial list of peer addresses to be returned as a `GetAddr` response.
///
/// The refresh path uses [`Mutex::try_lock`] on the underlying address book so it never blocks
/// the inbound service's hot path on the address book mutex; if the lock is contended we keep
/// serving the previous cached response.
pub struct CachedPeerAddrResponse {
    /// A direct handle to the underlying address book mutex used for the
    /// synchronous `try_lock` refresh fast path.
    inner_book: Arc<Mutex<AddressBook>>,

    /// An owned list of peer addresses used as a `GetAddr` response.
    value: zn::Response,

    /// Instant after which `cached_addrs` should be refreshed.
    refresh_time: Instant,
}

impl CachedPeerAddrResponse {
    /// Creates a new empty [`CachedPeerAddrResponse`].
    pub(super) fn new(address_book: AddressBookService) -> Self {
        let inner_book = address_book.shared();
        Self {
            inner_book,
            value: zn::Response::Nil,
            refresh_time: Instant::now(),
        }
    }

    pub(super) fn value(&self) -> zn::Response {
        self.value.clone()
    }

    /// Refreshes the `cached_addrs` if the time has past `refresh_time` or the cache is empty.
    pub(super) fn try_refresh(&mut self) {
        let now = Instant::now();

        // return early if there are some cached addresses, and they are still fresh
        if now < self.refresh_time {
            return;
        }

        let cache_expiry = self.refresh_time + CACHED_ADDRS_REFRESH_INTERVAL;

        // Use try_lock so the inbound service's hot path never blocks on the address book mutex.
        match self
            .inner_book
            .try_lock()
            .map(|book| book.fresh_get_addr_response())
        {
            // Update cached value and refresh_time if there are some gossipable peers in the address book.
            //
            // Security: this avoids outdated gossiped peers. Outdated Zebra binaries will gradually lose all their peers,
            // because those peers refuse to connect to outdated versions. So we don't want those outdated Zebra
            // versions to keep gossiping old peer information either.
            Ok(peers) if !peers.is_empty() => {
                self.refresh_time = now + CACHED_ADDRS_REFRESH_INTERVAL;
                self.value = zn::Response::Peers(peers);
            }

            // Clear the cached response if the time has past the cache expiry time.
            Ok(_) if now > cache_expiry => {
                self.value = zn::Response::Nil;
            }

            Err(TryLockError::WouldBlock) if now > cache_expiry => {
                warn!("getaddrs response hasn't been refreshed in some time");
                self.value = zn::Response::Nil;
            }

            // Don't update the cached response or refresh time if unable to get new peer addresses
            // from the address book and `now` is before the cache expiry.
            Ok(_) => {
                debug!(
                    "could not refresh cached response because our address \
                     book has no available peers"
                );
            }

            Err(TryLockError::WouldBlock) => {}

            // Panic if the address book lock is poisoned
            Err(TryLockError::Poisoned(_)) => {
                panic!("previous thread panicked while holding the address book lock")
            }
        };
    }

}
