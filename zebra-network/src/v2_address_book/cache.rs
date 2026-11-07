//! The version 2 peer cache: a second peer list file, `<cache_dir>/network/<network>.v2-peers`,
//! in the legacy peer cache format.

use std::{collections::HashSet, time::Duration};

use tokio::time::sleep;

use crate::{
    constants::{DNS_LOOKUP_TIMEOUT, PEER_DISK_CACHE_UPDATE_INTERVAL},
    Config, PeerSocketAddr,
};

use super::{Provenance, V2BookHandle};

/// Loads the version 2 peer cache for `config`, if it is enabled, and seeds `book` with it.
///
/// Returns the number of cached addresses. Errors are logged by the loader, and the cache is
/// optional, so they leave the segment as it was.
pub(crate) async fn load_v2_peer_cache(config: &Config, book: &V2BookHandle) -> usize {
    let Some(path) = config.cache_dir.v2_peer_cache_file_path(&config.network) else {
        return 0;
    };
    let addrs: Vec<PeerSocketAddr> = config
        .load_peer_cache_at(&path)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    let count = addrs.len();
    book.seed(addrs, Provenance::Cache).await;
    count
}

/// Regularly writes the segment's cacheable addresses, the peers this node connected to, to the
/// version 2 peer cache for `config`, if it is enabled, like the legacy peer cache updater.
pub(crate) async fn v2_peer_cache_updater(config: Config, book: V2BookHandle) {
    v2_peer_cache_updater_with(config, book, DNS_LOOKUP_TIMEOUT * 4).await
}

/// [`v2_peer_cache_updater`], waiting `first_wait` before the first write, and between writes
/// until the first one.
pub(super) async fn v2_peer_cache_updater_with(
    config: Config,
    book: V2BookHandle,
    first_wait: Duration,
) {
    let Some(path) = config.cache_dir.v2_peer_cache_file_path(&config.network) else {
        return;
    };

    // Wait until the configured and cached peers were (hopefully) dialled.
    sleep(first_wait).await;

    let mut wrote_cache = false;
    loop {
        let addrs: HashSet<PeerSocketAddr> = book
            .cacheable()
            .await
            .into_iter()
            .map(|meta_addr| meta_addr.addr)
            .collect();
        let has_addrs = !addrs.is_empty();

        // Updating the cache is optional, and the writer logs its errors.
        wrote_cache |= config
            .update_peer_cache_at(path.clone(), addrs)
            .await
            .is_ok()
            && has_addrs;

        // Right after a cold start, the segment can be empty, and the cache is only written once
        // it has cacheable addresses: retry soon until the first write.
        let interval = if wrote_cache {
            PEER_DISK_CACHE_UPDATE_INTERVAL
        } else {
            first_wait
        };
        sleep(interval).await;
    }
}
