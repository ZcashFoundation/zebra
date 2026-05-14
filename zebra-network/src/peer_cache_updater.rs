//! An async task that regularly updates the peer cache on disk from the current address book.

use std::io;

use tokio::time::sleep;
use tower::{Service, ServiceExt};

use crate::{
    address_book_service::{AddressBookRequest, AddressBookResponse},
    constants::{DNS_LOOKUP_TIMEOUT, PEER_DISK_CACHE_UPDATE_INTERVAL},
    meta_addr::MetaAddr,
    AddressBookService, BoxError, Config,
};

/// An ongoing task that regularly caches the current `address_book` to disk, based on `config`.
#[instrument(skip(config, address_book))]
pub async fn peer_cache_updater(
    config: Config,
    address_book: AddressBookService,
) -> Result<(), BoxError> {
    // Wait until we've queried DNS and (hopefully) sent peers to the address book.
    // Ideally we'd wait for at least one peer crawl, but that makes tests very slow.
    //
    // TODO: turn the initial sleep time into a parameter of this function,
    //       and allow it to be set in tests
    sleep(DNS_LOOKUP_TIMEOUT * 4).await;

    loop {
        // Ignore errors because updating the cache is optional.
        // Errors are already logged by the functions we're calling.
        let _ = update_peer_cache_once(&config, &address_book).await;

        sleep(PEER_DISK_CACHE_UPDATE_INTERVAL).await;
    }
}

/// Caches peers from the current `address_book` to disk, based on `config`.
pub async fn update_peer_cache_once(
    config: &Config,
    address_book: &AddressBookService,
) -> io::Result<()> {
    let peer_list = cacheable_peers(address_book)
        .await
        .iter()
        .map(|meta_addr| meta_addr.addr)
        .collect();

    config.update_peer_cache(peer_list).await
}

/// Returns a list of cacheable peers, blocking for as short a time as possible.
async fn cacheable_peers(address_book: &AddressBookService) -> Vec<MetaAddr> {
    let mut svc = address_book.clone();
    let result = match svc.ready().await {
        Ok(svc) => svc.call(AddressBookRequest::Cacheable).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(AddressBookResponse::Peers(peers)) => peers,
        Ok(other) => unreachable!("Cacheable returns Peers, got {other:?}"),
        Err(error) => {
            tracing::warn!(
                ?error,
                "address book service failed to return cacheable peers"
            );
            Vec::new()
        }
    }
}
