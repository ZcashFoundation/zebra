//! An async task that regularly updates the peer cache on disk from the current address book.

use std::io;

use futures::FutureExt;
use tokio::time::sleep;
use tower::ServiceExt;

use crate::{
    address_book_updater::{AddressBookRequest, AddressBookResponse, AddressBookService},
    constants::{DNS_LOOKUP_TIMEOUT, PEER_DISK_CACHE_UPDATE_INTERVAL},
    meta_addr::MetaAddr,
    BoxError, Config,
};

/// An ongoing task that regularly caches the current peers to disk, based on `config`.
#[instrument(skip(config, address_book_service))]
pub async fn peer_cache_updater(
    config: Config,
    address_book_service: AddressBookService,
) -> Result<(), BoxError> {
    // Wait until we've queried DNS and (hopefully) sent peers to the address book.
    // Ideally we'd wait for at least one peer crawl, but that makes tests very slow.
    //
    // TODO: turn the initial sleep time into a parameter of this function,
    //       and allow it to be set in tests
    sleep(DNS_LOOKUP_TIMEOUT * 4).await;

    let mut wrote_cache = false;

    loop {
        // Ignore errors because updating the cache is optional.
        // Errors are already logged by the functions we're calling.
        wrote_cache |= update_peer_cache_once(&config, address_book_service.clone())
            .await
            .unwrap_or(false);

        // Right after a cold start, the address book can still be empty at the first attempt,
        // and the cache is only written once there are cacheable peers. Retry soon until the
        // first write, so the cache exists shortly after the node finds its first peers.
        let interval = if wrote_cache {
            PEER_DISK_CACHE_UPDATE_INTERVAL
        } else {
            DNS_LOOKUP_TIMEOUT * 4
        };

        sleep(interval).await;
    }
}

/// Caches the current cacheable peers to disk, based on `config`.
///
/// Returns `true` if the cache was written, and `false` if the cacheable peer list was empty,
/// keeping any previous cache.
pub async fn update_peer_cache_once(
    config: &Config,
    address_book_service: AddressBookService,
) -> io::Result<bool> {
    let peer_list: std::collections::HashSet<_> = cacheable_peers(address_book_service)
        .await?
        .iter()
        .map(|meta_addr| meta_addr.addr)
        .collect();
    let has_peers = !peer_list.is_empty();

    config.update_peer_cache(peer_list).await?;

    Ok(has_peers)
}

/// Returns a list of cacheable peers from the address book updater task.
async fn cacheable_peers(address_book_service: AddressBookService) -> io::Result<Vec<MetaAddr>> {
    // Correctness: box the request future, so its captured types don't leak
    // into generic callers (rustc's async Send inference struggles with
    // boxed trait objects inside generic spawned tasks).
    match address_book_service
        .oneshot(AddressBookRequest::CacheablePeers)
        .boxed()
        .await
    {
        Ok(AddressBookResponse::Peers(peers)) => Ok(peers),
        Ok(_) => unreachable!("CacheablePeers requests always return Peers"),
        Err(error) => Err(io::Error::other(format!(
            "error requesting cacheable peers, is Zebra shutting down? {error}"
        ))),
    }
}
