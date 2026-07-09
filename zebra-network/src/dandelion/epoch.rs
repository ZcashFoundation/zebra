//! Dandelion++ epoch manager: per-epoch stem-peer selection.
//!
//! A new stem peer is selected uniformly at random from the connected outbound
//! peer set at the start of every epoch.  The epoch duration is randomised to
//! prevent an adversary from exploiting synchronised resets.

use std::time::Duration;

use rand::Rng;
use tokio::time::{sleep, Instant};
use tracing::{debug, info};

use crate::meta_addr::PeerSocketAddr;

/// Base epoch duration for Dandelion++ stem-peer rotation.
///
/// The Dandelion++ paper recommends ~10 minutes.
pub const EPOCH_DURATION: Duration = Duration::from_secs(10 * 60);

/// Maximum additional random jitter added to each epoch duration.
///
/// Jitter prevents all nodes from rotating their stem peer simultaneously,
/// which would allow an adversary to time attacks on epoch boundaries.
pub const EPOCH_JITTER: Duration = Duration::from_secs(2 * 60);

/// Manages the Dandelion++ epoch and exposes the current stem peer address.
///
/// # Usage
///
/// ```text
/// let mut manager = DandelionEpochManager::new();
/// loop {
///     manager.wait_for_epoch_end().await;
///     let peers = peer_set.outbound_addrs();
///     manager.rotate(&peers);
///     // use manager.stem_peer() in gossip_stem()
/// }
/// ```
#[derive(Debug)]
pub struct DandelionEpochManager {
    /// The address of the current epoch's stem peer, or `None` if no outbound
    /// peers are available (fluff mode).
    current_stem_peer: Option<PeerSocketAddr>,
    /// When the current epoch will end.
    epoch_end: Instant,
}

impl DandelionEpochManager {
    /// Creates a new manager.  The first epoch starts immediately with no stem
    /// peer (fluff mode until the first [`Self::rotate`] call).
    pub fn new() -> Self {
        Self {
            current_stem_peer: None,
            epoch_end: Instant::now(), // triggers rotation on first call
        }
    }

    /// Returns the current epoch's stem peer, or `None` if in fluff mode.
    pub fn stem_peer(&self) -> Option<PeerSocketAddr> {
        self.current_stem_peer
    }

    /// Returns `true` if the current epoch has expired.
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.epoch_end
    }

    /// Sleeps until the current epoch ends, then returns.
    pub async fn wait_for_epoch_end(&self) {
        let now = Instant::now();
        if self.epoch_end > now {
            sleep(self.epoch_end - now).await;
        }
    }

    /// Rotates to a new epoch, selecting a new stem peer at random from
    /// `outbound_peers`.  If the slice is empty, enters fluff mode for this
    /// epoch.
    ///
    /// This MUST be called after [`Self::wait_for_epoch_end`] returns.
    pub fn rotate(&mut self, outbound_peers: &[PeerSocketAddr]) {
        let jitter_secs = rand::thread_rng().gen_range(0..=EPOCH_JITTER.as_secs());
        let duration = EPOCH_DURATION + Duration::from_secs(jitter_secs);
        self.epoch_end = Instant::now() + duration;

        self.current_stem_peer = if outbound_peers.is_empty() {
            info!("dandelion++: no outbound peers, entering fluff mode for this epoch");
            None
        } else {
            let idx = rand::thread_rng().gen_range(0..outbound_peers.len());
            let chosen = outbound_peers[idx];
            debug!(%chosen, epoch_secs = duration.as_secs(), "dandelion++: new stem peer selected");
            Some(chosen)
        };
    }
}

impl Default for DandelionEpochManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn addrs(n: usize) -> Vec<PeerSocketAddr> {
        (0..n)
            .map(|i| PeerSocketAddr::from_str(&format!("127.0.0.1:{}", 8000 + i)).unwrap())
            .collect()
    }

    #[test]
    fn rotate_with_peers_selects_one() {
        let mut mgr = DandelionEpochManager::new();
        let peers = addrs(5);
        mgr.rotate(&peers);
        let stem = mgr.stem_peer().expect("should have a stem peer");
        assert!(peers.contains(&stem));
    }

    #[test]
    fn rotate_with_no_peers_is_fluff_mode() {
        let mut mgr = DandelionEpochManager::new();
        mgr.rotate(&[]);
        assert!(mgr.stem_peer().is_none());
    }

    #[test]
    fn new_epoch_is_immediately_expired() {
        // The default epoch_end is Instant::now() at construction time,
        // so is_expired() should be true immediately.
        let mgr = DandelionEpochManager::new();
        assert!(mgr.is_expired());
    }

    #[test]
    fn after_rotate_epoch_is_not_expired() {
        let mut mgr = DandelionEpochManager::new();
        mgr.rotate(&addrs(1));
        assert!(!mgr.is_expired());
    }
}
