//! Inventory Registry Implementation
//!
//! [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{FutureExt, Stream, StreamExt};
use indexmap::{IndexMap, IndexSet};
use tokio::{
    sync::{broadcast, watch, Semaphore},
    time::{self, Instant},
};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, IntervalStream};

use zebra_chain::{block, serialization::AtLeastOne};

use crate::{
    constants::INVENTORY_ROTATION_INTERVAL,
    protocol::{external::InventoryHash, internal::InventoryResponse},
    BoxError, PeerSocketAddr,
};

use self::update::Update;

/// Underlying type for the alias InventoryStatus::*
use InventoryResponse::*;

pub mod update;

#[cfg(test)]
mod tests;

/// The maximum number of inventory hashes we will track from a single peer.
///
/// # Security
///
/// This limits known memory denial of service attacks like <https://invdos.net/> to a total of:
/// ```text
/// 1000 inventory * 2 maps * 32-64 bytes per inventory = less than 1 MB
/// 1000 inventory * 70 peers * 2 maps * 6-18 bytes per address = up to 3 MB
/// ```
///
/// Since the inventory registry is an efficiency optimisation, which falls back to a
/// random peer, we only need to track a small number of hashes for available inventory.
///
/// But we want to be able to track a significant amount of missing inventory,
/// to limit queries for globally missing inventory.
//
// TODO: split this into available (25) and missing (1000 or more?)
pub const MAX_INV_PER_MAP: usize = 1000;

/// The maximum number of peers we will track inventory for.
///
/// # Security
///
/// This limits known memory denial of service attacks. See [`MAX_INV_PER_MAP`] for details.
///
/// Since the inventory registry is an efficiency optimisation, which falls back to a
/// random peer, we only need to track a small number of peers per inv for available inventory.
///
/// But we want to be able to track missing inventory for almost all our peers,
/// so we only query a few peers for inventory that is genuinely missing from the network.
//
// TODO: split this into available (25) and missing (70)
pub const MAX_PEERS_PER_INV: usize = 70;

/// A peer inventory status, which tracks a hash for both available and missing inventory.
pub type InventoryStatus<T> = InventoryResponse<T, T>;

/// A peer inventory status change, used in the inventory status channel.
///
/// For performance reasons, advertisements should only be tracked
/// for hashes that are rare on the network.
/// So Zebra only tracks single-block inventory messages.
///
/// For security reasons, all `notfound` rejections should be tracked.
/// This also helps with performance, if the hash is rare on the network.
pub type InventoryChange = InventoryStatus<(AtLeastOne<InventoryHash>, PeerSocketAddr)>;

/// An internal marker used in inventory status hash maps.
type InventoryMarker = InventoryStatus<()>;

/// The number of repeat requests for a block that were sent to each group of peers, see
/// [`InventoryRegistry::repeat_requests()`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RepeatRequests {
    /// Requests sent to peers that advertised the block.
    pub advertisers: usize,

    /// Requests sent to peers that didn't advertise the block.
    pub other_peers: usize,
}

/// An Inventory Registry for tracking recent inventory advertisements and missing inventory.
///
/// For more details please refer to the [RFC].
///
/// [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html
pub struct InventoryRegistry {
    /// Map tracking the latest inventory status from the current interval
    /// period.
    //
    // TODO: split maps into available and missing, so we can limit them separately.
    current: IndexMap<InventoryHash, IndexMap<PeerSocketAddr, InventoryMarker>>,

    /// Map tracking inventory statuses from the previous interval period.
    prev: IndexMap<InventoryHash, IndexMap<PeerSocketAddr, InventoryMarker>>,

    /// First block announcers in the current interval, capped by [`MAX_INV_PER_MAP`].
    /// Missing statuses must not erase first-announcer history.
    announced_current: IndexMap<block::Hash, PeerSocketAddr>,

    /// First block announcers in the previous interval, with the same cap.
    announced_prev: IndexMap<block::Hash, PeerSocketAddr>,

    /// The last committed tip considered for source credit; repeated inventory cannot refresh it.
    compact_tip: Option<block::Hash>,

    /// Map tracking peers that failed to return inventory in the current interval period.
    ///
    /// A failure is a completed peer error or response timeout, not caller cancellation or a
    /// `notfound` response. Missing inventory is tracked separately; failed peers might still have it.
    ///
    /// Failures are tracked separately from inventory statuses, so a peer can't clear its failure
    /// by advertising the inventory again.
    ///
    /// # Security
    ///
    /// This map is limited like the status maps, using [`MAX_INV_PER_MAP`] and
    /// [`MAX_PEERS_PER_INV`]. So it uses up to 3 MB for both intervals.
    failed_current: IndexMap<InventoryHash, IndexSet<PeerSocketAddr>>,

    /// Map tracking peers that failed to return inventory in the previous interval period.
    failed_prev: IndexMap<InventoryHash, IndexSet<PeerSocketAddr>>,

    /// Map tracking the peers that returned a block they were asked for because the block was
    /// advertised, in the current interval period.
    ///
    /// # Security
    ///
    /// A caller only asks for a block again if it rejected the block it was given. So a second
    /// request for one of these blocks shows that a peer that advertised it served something
    /// else, and the advertisements for it can't be trusted. See
    /// [`InventoryRegistry::served_peers()`].
    ///
    /// This map is limited like `failed_current`.
    served_current: IndexMap<InventoryHash, IndexSet<PeerSocketAddr>>,

    /// Map tracking the peers that returned an advertised block in the previous interval period.
    served_prev: IndexMap<InventoryHash, IndexSet<PeerSocketAddr>>,

    /// Map tracking the repeat requests sent for a block in the current interval period, see
    /// [`InventoryRegistry::repeat_requests()`].
    ///
    /// This map is limited like `failed_current`.
    repeat_current: IndexMap<InventoryHash, RepeatRequests>,

    /// Map tracking the repeat requests sent for a block in the previous interval period.
    repeat_prev: IndexMap<InventoryHash, RepeatRequests>,

    /// Stream of incoming inventory statuses to register.
    inv_stream: Pin<
        Box<dyn Stream<Item = Result<InventoryChange, BroadcastStreamRecvError>> + Send + 'static>,
    >,

    /// Interval tracking when we should next rotate our maps.
    interval: IntervalStream,

    /// Connected opted-in peers, ordered by their most recent first committed-block announcement.
    compact_sources: IndexMap<PeerSocketAddr, watch::Sender<Option<Arc<Semaphore>>>>,
    /// Demotion releases a connection's slot before its low-bandwidth write.
    compact_receive_slots: Arc<Semaphore>,
}

impl std::fmt::Debug for InventoryRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InventoryRegistry")
            .field("current", &self.current)
            .field("prev", &self.prev)
            .finish()
    }
}

impl InventoryChange {
    /// Returns a new available inventory change from a single hash.
    pub fn new_available(hash: InventoryHash, peer: PeerSocketAddr) -> Self {
        let bv = AtLeastOne::from_vec(vec![hash]).expect("bounded vec must fit");
        InventoryStatus::Available((bv, peer))
    }

    /// Returns a new missing inventory change from a single hash.
    #[allow(dead_code)]
    pub fn new_missing(hash: InventoryHash, peer: PeerSocketAddr) -> Self {
        let bv = AtLeastOne::from_vec(vec![hash]).expect("bounded vec must fit");
        InventoryStatus::Missing((bv, peer))
    }

    /// Returns a new available multiple inventory change, if `hashes` contains at least one change.
    pub fn new_available_multi<'a>(
        hashes: impl IntoIterator<Item = &'a InventoryHash>,
        peer: PeerSocketAddr,
    ) -> Option<Self> {
        let mut hashes: Vec<InventoryHash> = hashes.into_iter().copied().collect();

        // # Security
        //
        // Don't send more hashes than we're going to store.
        // It doesn't matter which hashes we choose, because this is an efficiency optimisation.
        //
        //  This limits known memory denial of service attacks to:
        // `1000 hashes * 200 peers/channel capacity * 32-64 bytes = up to 12 MB`
        hashes.truncate(MAX_INV_PER_MAP);

        let hashes = hashes.try_into().ok();

        hashes.map(|hashes| InventoryStatus::Available((hashes, peer)))
    }

    /// Returns a new missing multiple inventory change, if `hashes` contains at least one change.
    pub fn new_missing_multi<'a>(
        hashes: impl IntoIterator<Item = &'a InventoryHash>,
        peer: PeerSocketAddr,
    ) -> Option<Self> {
        let mut hashes: Vec<InventoryHash> = hashes.into_iter().copied().collect();

        // # Security
        //
        // Don't send more hashes than we're going to store.
        // It doesn't matter which hashes we choose, because this is an efficiency optimisation.
        hashes.truncate(MAX_INV_PER_MAP);

        let hashes = hashes.try_into().ok();

        hashes.map(|hashes| InventoryStatus::Missing((hashes, peer)))
    }
}

impl<T> InventoryStatus<T> {
    /// Get a marker for the status, without any associated data.
    pub fn marker(&self) -> InventoryMarker {
        self.as_ref().map(|_inner| ())
    }

    /// Maps an `InventoryStatus<T>` to `InventoryStatus<U>` by applying a function to a contained value.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> InventoryStatus<U> {
        // Based on Option::map from https://doc.rust-lang.org/src/core/option.rs.html#844
        match self {
            Available(item) => Available(f(item)),
            Missing(item) => Missing(f(item)),
        }
    }
}

impl<T: Clone> InventoryStatus<T> {
    /// Returns a clone of the inner item, regardless of status.
    pub fn to_inner(&self) -> T {
        match self {
            Available(item) | Missing(item) => item.clone(),
        }
    }
}

impl InventoryRegistry {
    /// Returns a new Inventory Registry for `inv_stream`.
    pub fn new(inv_stream: broadcast::Receiver<InventoryChange>) -> Self {
        let interval = INVENTORY_ROTATION_INTERVAL;

        // Don't do an immediate rotation, current and prev are already empty.
        let mut interval = tokio::time::interval_at(Instant::now() + interval, interval);
        // # Security
        //
        // Delay missed ticks so newly registered inventory survives a full interval.
        // Per-map caps bound memory use even when polling is delayed.
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        Self {
            current: Default::default(),
            prev: Default::default(),
            announced_current: IndexMap::new(),
            announced_prev: IndexMap::new(),
            compact_tip: None,
            failed_current: Default::default(),
            failed_prev: Default::default(),
            served_current: Default::default(),
            served_prev: Default::default(),
            repeat_current: Default::default(),
            repeat_prev: Default::default(),
            inv_stream: BroadcastStream::new(inv_stream).boxed(),
            interval: IntervalStream::new(interval),
            compact_sources: IndexMap::new(),
            compact_receive_slots: Arc::new(Semaphore::new(3)),
        }
    }

    /// Register a bilaterally opted-in connection as a potential compact receive source.
    pub(super) fn add_compact_source(
        &mut self,
        peer: PeerSocketAddr,
        preference: watch::Sender<Option<Arc<Semaphore>>>,
    ) {
        self.compact_sources.insert(peer, preference);
    }

    /// Credit first announcers only after their block is committed, then select three sources.
    pub(super) fn select_compact_sources(
        &mut self,
        best_tip: Option<block::Hash>,
        connected: impl Fn(&PeerSocketAddr) -> bool,
    ) {
        if self.compact_tip != best_tip {
            self.compact_tip = best_tip;
            if let Some(peer) = best_tip.and_then(|hash| {
                self.announced_prev
                    .get(&hash)
                    .or_else(|| self.announced_current.get(&hash))
            }) {
                if let Some(index) = self.compact_sources.get_index_of(peer) {
                    self.compact_sources.move_index(index, 0);
                }
            }
        }
        self.compact_sources.retain(|peer, preference| {
            let retain = connected(peer) && !preference.is_closed();
            if !retain {
                preference.send_replace(None);
            }
            retain
        });
        for (index, preference) in self.compact_sources.values().enumerate() {
            preference.send_if_modified(|slots| {
                let selected = index < 3;
                if slots.is_some() == selected {
                    return false;
                }
                *slots = selected.then(|| self.compact_receive_slots.clone());
                true
            });
        }
    }

    /// Returns an iterator over addrs of peers that have recently advertised `hash` in their inventory.
    pub fn advertising_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &PeerSocketAddr> {
        self.status_peers(hash)
            .filter_map(|addr_status| addr_status.available())
    }

    /// Returns an iterator over addrs of peers that have recently missed `hash` in their inventory.
    pub fn missing_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &PeerSocketAddr> {
        self.status_peers(hash)
            .filter_map(|addr_status| addr_status.missing())
    }

    /// Returns an iterator over addrs of peers that recently failed to return `hash`.
    ///
    /// Can include the same peer twice, if it failed in the current and previous interval.
    pub fn failed_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &PeerSocketAddr> {
        self.failed_current
            .get(&hash)
            .into_iter()
            .chain(self.failed_prev.get(&hash))
            .flatten()
    }

    /// Records that `peer` failed to return `hash`.
    ///
    /// Failures expire after the registry rotates twice, like inventory statuses.
    pub fn register_failed(&mut self, hash: InventoryHash, peer: PeerSocketAddr) {
        Self::register_peer(&mut self.failed_current, hash, peer);
    }

    /// Returns an iterator over addrs of peers that recently returned `hash` when they were asked
    /// for it because it was advertised, or in a repeat request.
    ///
    /// Can include the same peer twice, if it returned `hash` in the current and previous interval.
    ///
    /// # Security
    ///
    /// If this isn't empty, a request for `hash` is a repeat: the caller rejected the block one
    /// of these peers gave it. A block hash only commits to the header, so a peer can advertise a
    /// block and then serve its header with other transactions. So the caller should only ask
    /// these peers as a last resort.
    ///
    /// Several peers can advertise the same block to get each retry in turn, and the caller can't
    /// tell them from the honest peers that advertised it. See
    /// [`InventoryRegistry::repeat_answer_count()`].
    pub fn served_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &PeerSocketAddr> {
        self.served_current
            .get(&hash)
            .into_iter()
            .chain(self.served_prev.get(&hash))
            .flatten()
    }

    /// Records that `peer` returned `hash` when it was asked for it because it was advertised, or
    /// in a repeat request.
    ///
    /// These records expire after the registry rotates twice, like inventory statuses.
    pub fn register_served(&mut self, hash: InventoryHash, peer: PeerSocketAddr) {
        Self::register_peer(&mut self.served_current, hash, peer);
    }

    /// Returns how many repeat requests for `hash` were recently sent to the peers that
    /// advertised it, and to the peers that didn't.
    ///
    /// # Security
    ///
    /// The peers that advertised `hash` and the peers that didn't take turns to get the repeat
    /// requests for it, and these counts decide whose turn it is. A peer that forges blocks can
    /// advertise them from several addresses, and it can also connect from addresses that don't
    /// advertise them and don't answer. So if either group got every repeat request, that group
    /// could keep them all from the honest peers.
    ///
    /// Requests are counted when they are sent, not when they are answered. So several repeat
    /// requests in flight at the same time take turns too, and a peer can't change the turn by
    /// delaying its answer.
    pub fn repeat_requests(&self, hash: InventoryHash) -> RepeatRequests {
        let current = self.repeat_current.get(&hash).copied().unwrap_or_default();
        let prev = self.repeat_prev.get(&hash).copied().unwrap_or_default();

        RepeatRequests {
            advertisers: current.advertisers.saturating_add(prev.advertisers),
            other_peers: current.other_peers.saturating_add(prev.other_peers),
        }
    }

    /// Records that a repeat request for `hash` was sent to a peer that advertised it, if
    /// `advertised` is true, or to a peer that didn't.
    ///
    /// These records expire after the registry rotates twice, like inventory statuses.
    pub fn register_repeat_request(&mut self, hash: InventoryHash, advertised: bool) {
        if let Some(index) = self.repeat_current.get_index_of(&hash) {
            self.repeat_current
                .move_index(index, self.repeat_current.len() - 1);
        }
        let requests = self.repeat_current.entry(hash).or_default();
        if advertised {
            requests.advertisers = requests.advertisers.saturating_add(1);
        } else {
            requests.other_peers = requests.other_peers.saturating_add(1);
        }

        // # Security
        //
        // Limit the number of stored hashes, removing the oldest entries, like `register_peer()`.
        if self.repeat_current.len() > MAX_INV_PER_MAP {
            // Performance: `MAX_INV_PER_MAP` is small, so O(n) performance is acceptable.
            self.repeat_current.shift_remove_index(0);
        }
    }

    /// Adds `peer` to the peers for `hash` in `map`, as the newest entry.
    fn register_peer(
        map: &mut IndexMap<InventoryHash, IndexSet<PeerSocketAddr>>,
        hash: InventoryHash,
        peer: PeerSocketAddr,
    ) {
        if let Some(index) = map.get_index_of(&hash) {
            map.move_index(index, map.len() - 1);
        }
        let peers = map.entry(hash).or_default();
        let (index, _) = peers.insert_full(peer);
        peers.move_index(index, peers.len() - 1);

        // # Security
        //
        // Limit the number of stored peers and hashes, removing the oldest entries, like
        // `register()`.
        if peers.len() > MAX_PEERS_PER_INV {
            // Performance: `MAX_PEERS_PER_INV` is small, so O(n) performance is acceptable.
            peers.shift_remove_index(0);
        }
        if map.len() > MAX_INV_PER_MAP {
            // Performance: `MAX_INV_PER_MAP` is small, so O(n) performance is acceptable.
            map.shift_remove_index(0);
        }
    }

    /// Clears failures from previous connections that used `peer`'s transient address.
    pub fn clear_peer_failures(&mut self, peer: PeerSocketAddr) {
        for failures in [&mut self.failed_current, &mut self.failed_prev] {
            failures.retain(|_, peers| {
                peers.shift_remove(&peer);
                !peers.is_empty()
            });
        }
    }

    /// Returns an iterator over peer inventory statuses for `hash`.
    ///
    /// Prefers current statuses to previously rotated statuses for the same peer.
    pub fn status_peers(
        &self,
        hash: InventoryHash,
    ) -> impl Iterator<Item = InventoryStatus<&PeerSocketAddr>> {
        let prev = self.prev.get(&hash);
        let current = self.current.get(&hash);

        // # Security
        //
        // Prefer `current` statuses for the same peer over previously rotated statuses.
        // This makes sure Zebra is using the most up-to-date network information.
        let prev = prev
            .into_iter()
            .flatten()
            .filter(move |(addr, _status)| !self.has_current_status(hash, **addr));
        let current = current.into_iter().flatten();

        current
            .chain(prev)
            .map(|(addr, status)| status.map(|()| addr))
    }

    /// Returns true if there is a current status entry for `hash` and `addr`.
    pub fn has_current_status(&self, hash: InventoryHash, addr: PeerSocketAddr) -> bool {
        self.current
            .get(&hash)
            .and_then(|current| current.get(&addr))
            .is_some()
    }

    /// Returns an iterator over peer inventory status hashes.
    ///
    /// Yields current statuses first, then previously rotated statuses.
    /// This can include multiple statuses for the same hash.
    #[allow(dead_code)]
    pub fn status_hashes(
        &self,
    ) -> impl Iterator<Item = (&InventoryHash, &IndexMap<PeerSocketAddr, InventoryMarker>)> {
        self.current.iter().chain(self.prev.iter())
    }

    /// Returns a future that waits for new registry updates.
    #[allow(dead_code)]
    pub fn update(&mut self) -> Update<'_> {
        Update::new(self)
    }

    /// Drive periodic inventory tasks.
    ///
    /// Rotates the inventory HashMaps on every timer tick.
    /// Drains the inv_stream channel and registers all advertised inventory.
    ///
    /// Returns an error if the inventory channel is closed.
    ///
    /// Otherwise, returns `Ok` if it performed at least one update or rotation, or `Poll::Pending`
    /// if there was no inventory change. Always registers a wakeup for the next inventory update
    /// or rotation, even when it returns `Ok`.
    pub fn poll_inventory(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        let mut result = Poll::Pending;

        // # Correctness
        //
        // Registers the current task for wakeup when the timer next becomes ready.
        // (But doesn't return, because we also want to register the task for wakeup when more
        // inventory arrives.)
        //
        // # Security
        //
        // Rotate before registering new inventory. Missed ticks restart the interval, so
        // repeated polls cannot immediately age out new advertisements or failures.
        if Pin::new(&mut self.interval).poll_next(cx).is_ready() {
            self.rotate();
            result = Poll::Ready(Ok(()));
        }

        // This module uses a broadcast channel instead of an mpsc channel, even
        // though there's a single consumer of inventory advertisements, because
        // the broadcast channel has ring-buffer behavior: when the channel is
        // full, sending a new message displaces the oldest message in the
        // channel.
        //
        // This is the behavior we want for inventory advertisements, because we
        // want to have a bounded buffer of unprocessed advertisements, and we
        // want to prioritize new inventory (which is likely only at a specific
        // peer) over old inventory (which is likely more widely distributed).
        //
        // The broadcast channel reports dropped messages by returning
        // `RecvError::Lagged`. It's crucial that we handle that error here
        // rather than propagating it through the peer set's Service::poll_ready
        // implementation, where reporting a failure means reporting a permanent
        // failure of the peer set.

        // Returns Pending if all messages are processed, but the channel could get more.
        loop {
            let channel_result = self.inv_stream.next().poll_unpin(cx);

            match channel_result {
                Poll::Ready(Some(Ok(change))) => {
                    self.register(change);
                    result = Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(count)))) => {
                    // This isn't a fatal inventory error, it's expected behaviour when Zebra is
                    // under load from peers.
                    metrics::counter!("pool.inventory.dropped").increment(1);
                    metrics::counter!("pool.inventory.dropped.messages").increment(count);

                    // If this message happens a lot, we should improve inventory registry
                    // performance, or poll the registry or peer set in a separate task.
                    info!(count, "dropped lagged inventory advertisements");
                }
                Poll::Ready(None) => {
                    // If the channel is empty and returns None, all senders, including the one in
                    // the handshaker, have been dropped, which really is a permanent failure.
                    result = Poll::Ready(Err(broadcast::error::RecvError::Closed.into()));
                }
                Poll::Pending => {
                    break;
                }
            }
        }

        result
    }

    /// Record the given inventory `change` for the peer `addr`.
    ///
    /// `Missing` markers are not updated until the registry rotates, for security reasons.
    fn register(&mut self, change: InventoryChange) {
        let new_status = change.marker();
        let (invs, addr) = change.to_inner();

        for inv in invs {
            use InventoryHash::*;
            assert!(
                matches!(inv, Block(_) | Tx(_) | Wtx(_)),
                "unexpected inventory type: {inv:?} from peer: {addr:?}",
            );

            if let Block(hash) = inv {
                if new_status.is_available() {
                    let first = self.announced_prev.get(&hash).copied().unwrap_or(addr);
                    self.announced_current.entry(hash).or_insert(first);
                    if self.announced_current.len() > MAX_INV_PER_MAP {
                        self.announced_current.shift_remove_index(0);
                    }
                }
            }

            let hash_peers = self.current.entry(inv).or_default();

            // # Security
            //
            // Prefer `missing` over `advertised`, so malicious peers can't reset their own entries,
            // and funnel multiple failing requests to themselves.
            if let Some(old_status) = hash_peers.get(&addr) {
                if old_status.is_missing() && new_status.is_available() {
                    debug!(?new_status, ?old_status, ?addr, ?inv, "skipping new status");
                    continue;
                }

                debug!(
                    ?new_status,
                    ?old_status,
                    ?addr,
                    ?inv,
                    "keeping both new and old status"
                );
            }

            let replaced_status = hash_peers.insert(addr, new_status);

            debug!(
                ?new_status,
                ?replaced_status,
                ?addr,
                ?inv,
                "inserted new status"
            );

            // # Security
            //
            // Limit the number of stored peers per hash, removing the oldest entries,
            // because newer entries are likely to be more relevant.
            //
            // TODO: do random or weighted random eviction instead?
            if hash_peers.len() > MAX_PEERS_PER_INV {
                // Performance: `MAX_PEERS_PER_INV` is small, so O(n) performance is acceptable.
                hash_peers.shift_remove_index(0);
            }

            // # Security
            //
            // Limit the number of stored inventory hashes, removing the oldest entries,
            // because newer entries are likely to be more relevant.
            //
            // TODO: do random or weighted random eviction instead?
            if self.current.len() > MAX_INV_PER_MAP {
                // Performance: `MAX_INV_PER_MAP` is small, so O(n) performance is acceptable.
                self.current.shift_remove_index(0);
            }
        }
    }

    /// Replace the prev HashMap with current's and replace current with an empty
    /// HashMap
    fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
        self.announced_prev = std::mem::take(&mut self.announced_current);
        self.failed_prev = std::mem::take(&mut self.failed_current);
        self.served_prev = std::mem::take(&mut self.served_current);
        self.repeat_prev = std::mem::take(&mut self.repeat_current);
    }
}
