//! The known-hash list as a [`HashSource`]: fetches 50,000-height
//! chunks with `SyncPeer::get_hashes`, reassembles truncated responses across
//! peers, verifies each against its pin with
//! `KnownHashListSpec::verify_chunk`, and discards mismatches with no
//! penalty. At most 4 chunks are resident.
//!
//! Each chunk is fetched by its own task. A fetch asks one peer for the
//! chunk, at most [`MAX_HASHES_PER_REQUEST`] heights at a time, and keeps
//! asking that peer while it answers in full. If the peer fails, or answers
//! with a shorter prefix, the fetch asks other peers for the rest. The
//! reassembled chunk is then verified. On a mismatch, the fetch discards the
//! chunk and starts again from its first height, without the peer that
//! supplied it, or that supplied its prefix if it was reassembled (zips#1344,
//! "Tolerance of divergent views": a peer is never penalized for a chunk that
//! fails its pin).
//!
//! When every peer has been tried, the fetch asks only peers that connect
//! later, until [`CHUNK_RETRY_INTERVAL`] has passed. It then tries every peer
//! again, starting at a random one, and waits twice as long after each round
//! that fails, up to [`MAX_CHUNK_RETRY_INTERVAL`]. So peers that refuse,
//! fail, or serve a wrong chunk are asked again at most once per round,
//! however often the peer set changes.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    future::Future,
    hash::Hash,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::{
    sync::watch,
    task::{AbortHandle, JoinSet},
};

use zebra_chain::{
    block::{self, SyncHashEntry},
    parameters::{
        known_hashes::{KnownHashError, KnownHashListSpec},
        Network,
    },
    BoxError,
};
use zebra_network::{sync_peer::ConnectionId, SyncPeer, SyncPeerError};

use super::scheduler::HashSource;

#[cfg(test)]
mod tests;

/// The most chunks resident or being fetched at once: about 8 MB of entries.
///
/// [`KnownHashes::cumulative_txouts`] shares this budget with the chunks
/// resident or being fetched for the engine, but always fetches at least one
/// chunk at a time, so at most one chunk more is held while it runs.
pub const MAX_RESIDENT_CHUNKS: usize = 4;

/// How long a chunk fetch waits, after every peer failed to provide a chunk
/// that matches its pin, before it tries every peer again. Peers that connect
/// in the meantime are asked at once.
pub const CHUNK_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// The longest a chunk fetch waits between rounds: the
/// [`CHUNK_RETRY_INTERVAL`] doubles after each failed round, up to this.
pub const MAX_CHUNK_RETRY_INTERVAL: Duration = Duration::from_secs(8 * 30);

/// The most heights asked for in one `get-hashes` request: about 400 KB of
/// entries, which a responder that abandons slow responses after 5 seconds
/// sends to a reader of about 80 KB/s. A full chunk takes five requests.
pub const MAX_HASHES_PER_REQUEST: u32 = 10_000;

/// The known-hash list, fetched on demand from version 2 peers.
///
/// Generic over the peer type only for tests. The engine uses the default,
/// [`SyncPeer`].
pub struct KnownHashes<P = SyncPeer> {
    /// The chunk layout and the pins.
    pins: Arc<dyn ChunkPins>,

    /// The connected version 2 peers.
    peers: watch::Receiver<Arc<[P]>>,

    /// The verified chunks, by chunk index.
    resident: BTreeMap<usize, Arc<[SyncHashEntry]>>,

    /// The chunks being fetched, with their tasks' abort handles. An aborted
    /// fetch is removed here at once, so its cancellation is ignored when it
    /// is joined.
    fetching: BTreeMap<usize, AbortHandle>,

    /// The chunk fetch tasks, which return their chunk index.
    tasks: JoinSet<(usize, Result<Arc<[SyncHashEntry]>, BoxError>)>,

    /// Why the source failed for good, if it has.
    failed: Option<String>,
}

impl KnownHashes {
    /// A source for `spec` on `network`, fetching from `peers`.
    pub fn new(spec: KnownHashListSpec, network: Network, peers: zebra_network::SyncPeers) -> Self {
        Self::with_pins(Arc::new(PinnedList { spec, network }), peers)
    }
}

impl<P: HashPeer> KnownHashes<P> {
    /// A source for `pins`, fetching from `peers`.
    pub(crate) fn with_pins(pins: Arc<dyn ChunkPins>, peers: watch::Receiver<Arc<[P]>>) -> Self {
        KnownHashes {
            pins,
            peers,
            resident: BTreeMap::new(),
            fetching: BTreeMap::new(),
            tasks: JoinSet::new(),
            failed: None,
        }
    }

    /// The sum of `txouts` over the verified entries for heights
    /// `0..=through`, fetching and verifying every chunk up to `through`
    /// (without keeping them resident): the number of transparent outputs a
    /// spentness-hint artifact for `through` has a bit for.
    ///
    /// Reuses the chunks that are already resident, and waits for the ones
    /// the source is already fetching, which then stay resident. Fetches the
    /// others within what is left of [`MAX_RESIDENT_CHUNKS`] after the
    /// source's resident chunks, and at least one at a time.
    ///
    /// # Errors
    ///
    /// If `through` is above `max_height`, or the chunks cannot be verified
    /// from the available peers.
    pub async fn cumulative_txouts(&mut self, through: block::Height) -> Result<u64, BoxError> {
        let last = self.pins.chunk_index_for(through).ok_or_else(|| {
            format!(
                "height {through:?} is above the known-hash list's maximum height {:?}",
                self.pins.max_height()
            )
        })?;
        let last_start = chunk_start(&*self.pins, last)?;
        // `through` is in chunk `last`, so it is at or above the chunk's start.
        let last_take = usize::try_from(through.0 - last_start.0 + 1)?;

        // Fetch tasks are joined in completion order, so this can also make other chunks
        // resident, as `changed` would.
        while self.failed.is_none() && self.fetching.range(..=last).next().is_some() {
            if self.join_fetch().await.is_none() {
                break;
            }
        }

        let concurrency = MAX_RESIDENT_CHUNKS.saturating_sub(self.occupied()).max(1);
        let resident = &self.resident;
        let pins = &self.pins;
        let peers = &self.peers;
        stream::iter(0..=last)
            .map(|index| async move {
                match resident.get(&index) {
                    Some(chunk) => Ok(chunk.clone()),
                    None => fetch_chunk(pins.clone(), peers.clone(), index).await,
                }
            })
            .buffered(concurrency)
            .enumerate()
            .map(|(index, chunk)| chunk.map(|chunk| (index, chunk)))
            .try_fold(0_u64, |total, (index, chunk)| async move {
                let take = if index == last {
                    last_take
                } else {
                    chunk.len()
                };
                chunk[..take]
                    .iter()
                    .try_fold(total, |total, entry| total.checked_add(entry.txouts))
                    .ok_or_else(|| BoxError::from("the cumulative txouts overflow a u64"))
            })
            .await
    }

    /// Starts fetching chunk `index`.
    fn spawn_fetch(&mut self, index: usize) {
        let pins = self.pins.clone();
        let peers = self.peers.clone();
        let task = self.tasks.spawn(async move {
            let chunk = fetch_chunk(pins, peers, index).await;
            (index, chunk)
        });
        self.fetching.insert(index, task);
    }

    /// Makes room for one more chunk by dropping the lowest chunk below
    /// `first`, resident or being fetched. Returns whether it made room.
    fn evict_below(&mut self, first: usize) -> bool {
        let lowest = self
            .resident
            .keys()
            .chain(self.fetching.keys())
            .copied()
            .min()
            .filter(|&index| index < first);
        let Some(index) = lowest else {
            return false;
        };
        self.resident.remove(&index);
        if let Some(task) = self.fetching.remove(&index) {
            task.abort();
        }
        true
    }

    /// Joins the next chunk fetch to finish. Returns whether it made a chunk
    /// resident, or `None` if nothing is being fetched.
    ///
    /// A failed fetch fails the source for good: the registry has stopped.
    async fn join_fetch(&mut self) -> Option<bool> {
        let joined = self.tasks.join_next().await?;
        let (index, chunk) = match joined {
            Ok(joined) => joined,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            // An evicted or released fetch, which was removed from `fetching`.
            Err(_cancelled) => return Some(false),
        };
        if self.fetching.remove(&index).is_none() {
            // A fetch that finished after it was evicted or released, but before its abort.
            return Some(false);
        }
        match chunk {
            Ok(chunk) => {
                self.resident.insert(index, chunk);
                Some(true)
            }
            Err(error) => {
                tracing::info!(
                    %error,
                    index,
                    "known-hash list unavailable, \
                     continuing checkpoint sync with checkpoint anchors only"
                );
                self.failed = Some(error.to_string());
                self.fetching.clear();
                self.tasks.abort_all();
                Some(false)
            }
        }
    }

    /// The number of chunks resident or being fetched.
    fn occupied(&self) -> usize {
        self.resident.len() + self.fetching.len()
    }
}

impl<P: HashPeer> HashSource for KnownHashes<P> {
    fn max_height(&self) -> block::Height {
        self.pins.max_height()
    }

    fn entry(&self, height: block::Height) -> Option<SyncHashEntry> {
        let index = self.pins.chunk_index_for(height)?;
        let chunk = self.resident.get(&index)?;
        let start = self.pins.chunk_start(index)?;
        let offset = usize::try_from(height.0.checked_sub(start.0)?).ok()?;
        chunk.get(offset).copied()
    }

    fn request(&mut self, start: block::Height, end: block::Height) {
        if self.failed.is_some() || end < start {
            return;
        }
        let Some(first) = self.pins.chunk_index_for(start) else {
            return;
        };
        let last = self
            .pins
            .chunk_index_for(end.min(self.pins.max_height()))
            .unwrap_or(first)
            .min(first + MAX_RESIDENT_CHUNKS - 1);

        for index in first..=last {
            if self.resident.contains_key(&index) || self.fetching.contains_key(&index) {
                continue;
            }
            if self.occupied() >= MAX_RESIDENT_CHUNKS && !self.evict_below(first) {
                break;
            }
            self.spawn_fetch(index);
        }
    }

    fn changed(&mut self) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + '_>> {
        Box::pin(async move {
            loop {
                if let Some(error) = &self.failed {
                    return Err(error.clone().into());
                }
                match self.join_fetch().await {
                    Some(true) => return Ok(()),
                    Some(false) => continue,
                    // Nothing is being fetched, so nothing can change until `request` is called.
                    None => return std::future::pending().await,
                }
            }
        })
    }

    fn release_below(&mut self, height: block::Height) {
        let pins = &*self.pins;
        let below =
            |index: &usize| chunk_end(pins, *index).is_some_and(|chunk_end| chunk_end < height);
        self.resident.retain(|index, _| !below(index));
        self.fetching.retain(|index, task| {
            let keep = !below(index);
            if !keep {
                task.abort();
            }
            keep
        });
    }
}

impl<P> fmt::Debug for KnownHashes<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KnownHashes")
            .field("max_height", &self.pins.max_height())
            .field("resident", &self.resident.keys().collect::<Vec<_>>())
            .field("fetching", &self.fetching.keys().collect::<Vec<_>>())
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

/// Fetches chunk `index` from `peers`, and returns it once it matches its pin.
///
/// Retries until it does. Fails only if the chunk index is out of range or
/// the peer registry has stopped.
async fn fetch_chunk<P: HashPeer>(
    pins: Arc<dyn ChunkPins>,
    mut peers: watch::Receiver<Arc<[P]>>,
    index: usize,
) -> Result<Arc<[SyncHashEntry]>, BoxError> {
    let start = chunk_start(&*pins, index)?;
    let len = pins
        .chunk_len(index)
        .ok_or_else(|| format!("known-hash chunk {index} is out of range"))?;
    let len_usize = usize::try_from(len)?;

    // The peers that failed to provide this chunk in this round: they refused, failed, had no
    // more entries, or supplied a chunk (or the prefix of a chunk) that did not match its pin.
    let mut tried = HashSet::new();
    let mut entries: Vec<SyncHashEntry> = Vec::new();
    let mut contributors = Vec::new();
    // Spreads the chunks over the peers, so concurrent fetches start at different peers.
    let mut rotation = index;
    // The peer that answered the last request in full, which is asked for the next part.
    let mut current: Option<P> = None;
    // When every peer has been tried: the end of the wait before the next round.
    let mut round_ends = None;
    let mut retry_interval = CHUNK_RETRY_INTERVAL;

    loop {
        if entries.len() == len_usize {
            match pins.verify_chunk(index, &entries) {
                Ok(()) => return Ok(entries.into()),
                Err(error) => {
                    // zips#1344 P:1979-1982: a mismatch is never attributed to a peer, which may
                    // have a different best chain. The chunk is discarded and fetched again from
                    // other peers.
                    tracing::debug!(
                        %error,
                        index,
                        ?contributors,
                        "discarding a known-hash chunk that does not match its pin"
                    );
                    //
                    // Only one peer is ruled out for the round. If one peer supplied the whole
                    // chunk, it is that peer. If the chunk was reassembled, it is the peer that
                    // supplied the prefix: the mismatch cannot tell which contributor diverged,
                    // and ruling out the peers that completed the prefix would let one peer
                    // that answers a short, wrong prefix rule out an honest peer every time.
                    // The chunk is then fetched again from offset 0, from a peer that did not
                    // supply the prefix.
                    if let Some(&prefix) = contributors.first() {
                        tried.insert(prefix);
                    }
                    contributors.clear();
                    entries.clear();
                    current = None;
                }
            }
        }

        let peer = {
            let connected = peers.borrow_and_update();
            let still_connected = current
                .take()
                .filter(|current| connected.iter().any(|peer| peer.id() == current.id()));
            still_connected.or_else(|| {
                let candidates: Vec<&P> = connected
                    .iter()
                    .filter(|peer| !tried.contains(&peer.id()))
                    .collect();
                if candidates.is_empty() {
                    None
                } else {
                    rotation = rotation.wrapping_add(1);
                    Some(candidates[rotation % candidates.len()].clone())
                }
            })
        };

        let Some(peer) = peer else {
            // Every peer has been tried in this round. Peers that connect before the round's
            // wait ends are tried at once; the others are tried again after it. Partial entries
            // are kept.
            let ends =
                *round_ends.get_or_insert_with(|| tokio::time::Instant::now() + retry_interval);
            match tokio::time::timeout_at(ends, peers.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_closed)) => return Err("the version 2 peer registry has stopped".into()),
                Err(_elapsed) => {
                    tried.clear();
                    round_ends = None;
                    retry_interval = (retry_interval * 2).min(MAX_CHUNK_RETRY_INTERVAL);
                    // Start the next round at a random peer, so the order of the picks in a
                    // round does not repeat from round to round.
                    // Truncating the random `u64` on a 32-bit target keeps it random.
                    rotation = rand::random::<u64>() as usize;
                }
            }
            continue;
        };

        let offset = u32::try_from(entries.len())?;
        // `offset < len`, and the chunk's heights fit in a `u32`.
        let next = block::Height(start.0 + offset);
        let count = (len - offset).min(MAX_HASHES_PER_REQUEST);
        match peer.get_hashes(next, count).await {
            Ok(fetched) if fetched.is_empty() => {
                tried.insert(peer.id());
            }
            Ok(fetched) => {
                // A conforming response has at most `count` entries, so `entries` stays within
                // the chunk.
                let full = fetched.len() == usize::try_from(count)?;
                entries.extend(fetched);
                entries.truncate(len_usize);
                if contributors.last() != Some(&peer.id()) {
                    contributors.push(peer.id());
                }
                if full {
                    // Ask the same peer for the next part of the chunk.
                    current = Some(peer);
                } else {
                    // The peer has no more entries: continue the prefix from other peers.
                    tried.insert(peer.id());
                }
            }
            Err(error) => {
                tracing::debug!(%error, index, peer = ?peer.id(), "get-hashes request failed");
                tried.insert(peer.id());
            }
        }
    }
}

/// The first height of chunk `index`.
fn chunk_start(pins: &dyn ChunkPins, index: usize) -> Result<block::Height, BoxError> {
    pins.chunk_start(index)
        .ok_or_else(|| format!("known-hash chunk {index} is out of range").into())
}

/// The last height of chunk `index`, or `None` past the last chunk.
fn chunk_end(pins: &dyn ChunkPins, index: usize) -> Option<block::Height> {
    let start = pins.chunk_start(index)?;
    let len = pins.chunk_len(index)?;
    // Chunks are never empty, and their heights fit in a `u32`.
    Some(block::Height(start.0 + len.checked_sub(1)?))
}

/// What [`KnownHashes`] needs from a peer: `get-hashes`.
pub trait HashPeer: Clone + Send + Sync + 'static {
    /// Identifies a peer, so a chunk fetch can skip the peers it already tried.
    type Id: Copy + Eq + Hash + fmt::Debug + Send + Sync + 'static;

    /// The peer's identifier.
    fn id(&self) -> Self::Id;

    /// [`SyncPeer::get_hashes`].
    fn get_hashes(
        &self,
        start: block::Height,
        count: u32,
    ) -> impl Future<Output = Result<Vec<SyncHashEntry>, SyncPeerError>> + Send;
}

impl HashPeer for SyncPeer {
    type Id = ConnectionId;

    fn id(&self) -> ConnectionId {
        SyncPeer::id(self)
    }

    fn get_hashes(
        &self,
        start: block::Height,
        count: u32,
    ) -> impl Future<Output = Result<Vec<SyncHashEntry>, SyncPeerError>> + Send {
        SyncPeer::get_hashes(self, start, count)
    }
}

/// What [`KnownHashes`] needs from a pinned list: its chunk layout, and the
/// check of a reassembled chunk. Implemented for [`KnownHashListSpec`] by
/// [`PinnedList`].
pub trait ChunkPins: Send + Sync + 'static {
    /// [`KnownHashListSpec::max_height`].
    fn max_height(&self) -> block::Height;

    /// [`KnownHashListSpec::chunk_index_for`].
    fn chunk_index_for(&self, height: block::Height) -> Option<usize>;

    /// [`KnownHashListSpec::chunk_start`].
    fn chunk_start(&self, index: usize) -> Option<block::Height>;

    /// [`KnownHashListSpec::chunk_len`].
    fn chunk_len(&self, index: usize) -> Option<u32>;

    /// [`KnownHashListSpec::verify_chunk`] on the list's network.
    fn verify_chunk(&self, index: usize, entries: &[SyncHashEntry]) -> Result<(), KnownHashError>;
}

/// A pinned [`KnownHashListSpec`] for one network.
#[derive(Clone, Debug)]
pub struct PinnedList {
    /// The pins.
    pub spec: KnownHashListSpec,

    /// The network the list is for.
    pub network: Network,
}

impl ChunkPins for PinnedList {
    fn max_height(&self) -> block::Height {
        self.spec.max_height
    }

    fn chunk_index_for(&self, height: block::Height) -> Option<usize> {
        self.spec.chunk_index_for(height)
    }

    fn chunk_start(&self, index: usize) -> Option<block::Height> {
        self.spec.chunk_start(index)
    }

    fn chunk_len(&self, index: usize) -> Option<u32> {
        self.spec.chunk_len(index)
    }

    fn verify_chunk(&self, index: usize, entries: &[SyncHashEntry]) -> Result<(), KnownHashError> {
        self.spec.verify_chunk(&self.network, index, entries)
    }
}

/// Known-hash list pins from config, for Regtest and custom testnets
/// (`[sync] v2_known_hash_pins`).
///
/// ```toml
/// [sync.v2_known_hash_pins]
/// max_height = 99_999
/// chunk_hashes = ["<64 hex digits>", "<64 hex digits>"]
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnownHashPins {
    /// The highest height the list covers.
    pub max_height: block::Height,

    /// The SHA-256 of each chunk's canonical bytes, in chunk order, as hex.
    pub chunk_hashes: Vec<ChunkPin>,
}

/// The SHA-256 of one known-hash chunk, written as 64 hex digits in byte
/// order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ChunkPin(pub [u8; 32]);

impl Serialize for ChunkPin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for ChunkPin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex_pin = String::deserialize(deserializer)?;
        let mut pin = [0; 32];
        hex::decode_to_slice(&hex_pin, &mut pin).map_err(|error| {
            serde::de::Error::custom(format!(
                "a known-hash chunk pin must be 64 hex digits: {error}"
            ))
        })?;
        Ok(ChunkPin(pin))
    }
}

/// The known-hash list for `network`: the compiled-in list on Mainnet and the
/// default Testnet, or the configured `pins` on Regtest and custom testnets.
///
/// Returns `None` if `network` has no list.
///
/// # Errors
///
/// If `pins` are configured on Mainnet or the default Testnet, or are
/// inconsistent.
pub fn known_hash_list(
    network: &Network,
    pins: Option<&KnownHashPins>,
) -> Result<Option<KnownHashListSpec>, KnownHashError> {
    match pins {
        None => Ok(KnownHashListSpec::for_network(network)),
        Some(pins) => KnownHashListSpec::from_config(
            network,
            pins.max_height,
            pins.chunk_hashes.iter().map(|pin| pin.0).collect(),
        )
        .map(Some),
    }
}
