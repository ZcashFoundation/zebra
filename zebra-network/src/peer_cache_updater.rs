//! An async task that regularly updates the peer cache on disk from the current peer book.

use std::io;

use tokio::time::sleep;
use tower::ServiceExt;

use crate::{
    constants::{DNS_LOOKUP_TIMEOUT, PEER_DISK_CACHE_UPDATE_INTERVAL},
    peer_book::{PeerBookHandle, PeerBookRequest, PeerBookResponse},
    BoxError, Config,
};

/// An ongoing task that regularly caches the current peer book to disk, based on `config`.
#[instrument(skip(config, peer_book_handle))]
pub async fn peer_cache_updater(
    config: Config,
    peer_book_handle: PeerBookHandle,
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
        wrote_cache |= update_peer_cache_once(&config, peer_book_handle.clone())
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

/// Caches the peer book's cacheable peers to disk, based on `config`.
///
/// Returns `true` if the cache was written, and `false` if the cacheable peer list was empty,
/// keeping any previous cache.
pub async fn update_peer_cache_once(
    config: &Config,
    peer_book_handle: PeerBookHandle,
) -> io::Result<bool> {
    let response = peer_book_handle
        .oneshot(PeerBookRequest::CacheSnapshot)
        .await
        .map_err(io::Error::other)?;
    let PeerBookResponse::Addrs(cacheable) = response else {
        return Err(io::Error::other("unexpected peer book response variant"));
    };

    let peer_list: std::collections::HashSet<_> =
        cacheable.iter().map(|meta_addr| meta_addr.addr).collect();
    let has_peers = !peer_list.is_empty();

    config.update_peer_cache(peer_list).await?;

    Ok(has_peers)
}
