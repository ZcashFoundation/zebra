//! Tests for the known-hash source and known-hash unit planning, with fake
//! peers and pins.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use tokio::sync::{mpsc, watch};

use zebra_chain::{
    block::{self, SyncHashEntry},
    parameters::known_hashes::KnownHashError,
};
use zebra_network::SyncPeerError;

use crate::components::checkpoint_sync::scheduler::{
    known_hash_unit, Anchor, HashSource, UnitId, DEEP_UNIT_BYTES, FRONTIER_UNIT_BYTES,
    MAX_UNIT_COUNT,
};

use super::*;

/// The longest any test waits for a source to change.
const TEST_TIMEOUT: Duration = Duration::from_secs(600);

/// The heights per chunk of the test lists.
const TEST_CHUNK_HEIGHTS: u32 = 10;

/// The honest entry at `height`: what every honest peer serves, and what the
/// test pins accept.
fn honest(height: u32) -> SyncHashEntry {
    let mut hash = [0; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    SyncHashEntry {
        hash: block::Hash(hash),
        size: 1_000 + u64::from(height),
        txs: 1,
        txouts: u64::from(height % 3) + 1,
        notes: 0,
    }
}

/// Test pins for heights `0..=max_height`, in `chunk_heights` chunks, that
/// accept exactly the [`honest`] entries.
struct TestPins {
    max_height: u32,
    chunk_heights: u32,
}

impl ChunkPins for TestPins {
    fn max_height(&self) -> block::Height {
        block::Height(self.max_height)
    }

    fn chunk_index_for(&self, height: block::Height) -> Option<usize> {
        if height.0 > self.max_height {
            return None;
        }
        usize::try_from(height.0 / self.chunk_heights).ok()
    }

    fn chunk_start(&self, index: usize) -> Option<block::Height> {
        let start = u32::try_from(index).ok()?.checked_mul(self.chunk_heights)?;
        (start <= self.max_height).then_some(block::Height(start))
    }

    fn chunk_len(&self, index: usize) -> Option<u32> {
        let start = self.chunk_start(index)?.0;
        Some(self.chunk_heights.min(self.max_height - start + 1))
    }

    fn verify_chunk(&self, index: usize, entries: &[SyncHashEntry]) -> Result<(), KnownHashError> {
        let start = self
            .chunk_start(index)
            .expect("tests verify valid chunks")
            .0;
        let expected = self.chunk_len(index).expect("tests verify valid chunks");
        if usize::try_from(expected) != Ok(entries.len()) {
            return Err(KnownHashError::ChunkLength {
                index,
                expected,
                actual: entries.len(),
            });
        }
        if entries
            .iter()
            .zip(start..)
            .any(|(entry, height)| *entry != honest(height))
        {
            return Err(KnownHashError::ChunkHashMismatch { index });
        }
        Ok(())
    }
}

/// How a [`FakePeer`] answers `get-hashes`.
#[derive(Copy, Clone, Debug)]
enum Behavior {
    /// Serves honest entries up to and including this height.
    HonestThrough(u32),
    /// Serves entries with wrong hashes.
    Lying,
    /// Serves entries with wrong hashes up to and including this height: a
    /// short, wrong prefix.
    LyingThrough(u32),
    /// Answers nothing for a while, then fails, like a stalled peer.
    Stalled,
}

/// One `get-hashes` request a [`FakePeer`] received.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Call {
    peer: u64,
    start: u32,
    count: u32,
}

/// A peer that answers `get-hashes` from [`honest`] entries, and records its
/// requests.
#[derive(Clone)]
struct FakePeer {
    id: u64,
    behavior: Behavior,
    calls: mpsc::UnboundedSender<Call>,
}

impl HashPeer for FakePeer {
    type Id = u64;

    fn id(&self) -> u64 {
        self.id
    }

    fn get_hashes(
        &self,
        start: block::Height,
        count: u32,
    ) -> impl Future<Output = Result<Vec<SyncHashEntry>, SyncPeerError>> + Send {
        let peer = self.clone();
        async move {
            let _ = peer.calls.send(Call {
                peer: peer.id,
                start: start.0,
                count,
            });
            let end = start.0 + count - 1;
            match peer.behavior {
                Behavior::HonestThrough(through) => {
                    Ok((start.0..=end.min(through)).map(honest).collect())
                }
                Behavior::Lying => Ok((start.0..=end)
                    .map(|height| SyncHashEntry {
                        hash: block::Hash([0xff; 32]),
                        ..honest(height)
                    })
                    .collect()),
                Behavior::LyingThrough(through) => Ok((start.0..=end.min(through))
                    .map(|height| SyncHashEntry {
                        hash: block::Hash([0xff; 32]),
                        ..honest(height)
                    })
                    .collect()),
                Behavior::Stalled => {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    Err(SyncPeerError::Transport("timed out".to_string()))
                }
            }
        }
    }
}

/// A test network: the peers' watch channel and their request log.
struct TestPeers {
    sender: watch::Sender<Arc<[FakePeer]>>,
    calls: mpsc::UnboundedReceiver<Call>,
    calls_sender: mpsc::UnboundedSender<Call>,
}

impl TestPeers {
    fn new() -> Self {
        let (sender, _) = watch::channel(Arc::from(Vec::new()));
        let (calls_sender, calls) = mpsc::unbounded_channel();
        TestPeers {
            sender,
            calls,
            calls_sender,
        }
    }

    fn peer(&self, id: u64, behavior: Behavior) -> FakePeer {
        FakePeer {
            id,
            behavior,
            calls: self.calls_sender.clone(),
        }
    }

    /// Replaces the connected peers.
    fn connect(&self, peers: Vec<FakePeer>) {
        self.sender.send_replace(peers.into());
    }

    /// A source for heights `0..=max_height`, in [`TEST_CHUNK_HEIGHTS`]
    /// chunks.
    fn source(&self, max_height: u32) -> KnownHashes<FakePeer> {
        self.source_with_chunks(max_height, TEST_CHUNK_HEIGHTS)
    }

    /// A source for heights `0..=max_height`, in `chunk_heights` chunks.
    fn source_with_chunks(&self, max_height: u32, chunk_heights: u32) -> KnownHashes<FakePeer> {
        KnownHashes::with_pins(
            Arc::new(TestPins {
                max_height,
                chunk_heights,
            }),
            self.sender.subscribe(),
        )
    }

    /// Waits for the next request.
    async fn next_call(&mut self) -> Call {
        tokio::time::timeout(TEST_TIMEOUT, self.calls.recv())
            .await
            .expect("a request arrives in time")
            .expect("the test holds a sender")
    }

    /// Every request received since the last call.
    fn drain_calls(&mut self) -> Vec<Call> {
        let mut calls = Vec::new();
        while let Ok(call) = self.calls.try_recv() {
            calls.push(call);
        }
        calls
    }
}

/// Waits for the next chunk to become resident.
async fn changed(source: &mut KnownHashes<FakePeer>) {
    tokio::time::timeout(TEST_TIMEOUT, source.changed())
        .await
        .expect("the source changes in time")
        .expect("the source has not failed");
}

/// Checks that heights `start..=end` are resident and honest.
fn assert_resident(source: &KnownHashes<FakePeer>, start: u32, end: u32) {
    for height in start..=end {
        assert_eq!(
            source.entry(block::Height(height)),
            Some(honest(height)),
            "height {height} is resident"
        );
    }
}

/// A chunk that does not match its pin is discarded and fetched again from
/// another peer. The liar is not penalized: [`HashPeer`] has no penalty, and
/// the liar is still asked for the next chunk.
#[tokio::test(start_paused = true)]
async fn lying_peer_is_discarded_and_retried_without_penalty() {
    let _init_guard = zebra_test::init();
    let mut peers = TestPeers::new();
    let liar = peers.peer(1, Behavior::Lying);
    let honest_peer = peers.peer(2, Behavior::HonestThrough(u32::MAX));

    let mut source = peers.source(19);
    for (start, end) in [(0, 9), (10, 19)] {
        // Only the liar is connected, so it answers first.
        peers.connect(vec![liar.clone()]);
        source.request(block::Height(start), block::Height(end));
        assert_eq!(
            peers.next_call().await,
            Call {
                peer: 1,
                start,
                count: 10
            }
        );
        tokio::task::yield_now().await;
        assert_eq!(
            source.entry(block::Height(start)),
            None,
            "the lie is discarded"
        );

        peers.connect(vec![liar.clone(), honest_peer.clone()]);
        changed(&mut source).await;
        assert_resident(&source, start, end);

        let calls = peers.drain_calls();
        assert!(
            calls.contains(&Call {
                peer: 2,
                start,
                count: 10
            }),
            "the chunk was fetched again from the honest peer: {calls:?}"
        );
    }
}

/// A liar that answers a short, wrong prefix does not keep a chunk from
/// verifying when an honest peer completes it: the mismatch rules out only
/// the liar, and the chunk is fetched again whole from the honest peer, in
/// the same round. Both peer orders are tried, so the liar is asked first in
/// one of them.
#[tokio::test(start_paused = true)]
async fn short_wrong_prefix_does_not_rule_out_the_honest_peer() {
    let _init_guard = zebra_test::init();

    for liar_first in [false, true] {
        let mut peers = TestPeers::new();
        let liar = peers.peer(1, Behavior::LyingThrough(3));
        let honest_peer = peers.peer(2, Behavior::HonestThrough(u32::MAX));
        peers.connect(if liar_first {
            vec![liar, honest_peer]
        } else {
            vec![honest_peer, liar]
        });

        let mut source = peers.source(9);
        source.request(block::Height(0), block::Height(9));
        tokio::time::timeout(CHUNK_RETRY_INTERVAL, source.changed())
            .await
            .expect("the chunk verifies without waiting for another round")
            .expect("the source has not failed");
        assert_resident(&source, 0, 9);

        let calls = peers.drain_calls();
        assert!(
            calls.contains(&Call {
                peer: 2,
                start: 0,
                count: 10
            }),
            "the honest peer supplied the whole chunk: {calls:?}"
        );
    }
}

/// A peer that answers with a prefix is continued from another peer, and the
/// reassembled chunk is verified.
#[tokio::test(start_paused = true)]
async fn truncated_responses_reassemble_across_peers() {
    let _init_guard = zebra_test::init();
    let mut peers = TestPeers::new();
    let short = peers.peer(1, Behavior::HonestThrough(13));
    let full = peers.peer(2, Behavior::HonestThrough(u32::MAX));
    peers.connect(vec![short.clone()]);

    let mut source = peers.source(29);
    source.request(block::Height(10), block::Height(19));

    assert_eq!(
        peers.next_call().await,
        Call {
            peer: 1,
            start: 10,
            count: 10
        }
    );
    tokio::task::yield_now().await;
    assert_eq!(
        source.entry(block::Height(10)),
        None,
        "a partial chunk is not resident"
    );

    peers.connect(vec![short, full]);
    changed(&mut source).await;
    assert_resident(&source, 10, 19);

    // The continuation starts after the prefix, and asks only for the rest.
    let calls = peers.drain_calls();
    assert!(
        calls.contains(&Call {
            peer: 2,
            start: 14,
            count: 6
        }),
        "the prefix is continued from the other peer: {calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.start == 10),
        "the prefix is not fetched again: {calls:?}"
    );
}

/// A chunk larger than [`MAX_HASHES_PER_REQUEST`] is fetched in parts of at
/// most that many heights, from the same peer while it answers in full.
#[tokio::test(start_paused = true)]
async fn full_responses_continue_from_the_same_peer() {
    let _init_guard = zebra_test::init();
    let mut peers = TestPeers::new();
    peers.connect(vec![
        peers.peer(1, Behavior::HonestThrough(u32::MAX)),
        peers.peer(2, Behavior::HonestThrough(u32::MAX)),
    ]);

    let chunk_heights = 2 * MAX_HASHES_PER_REQUEST + MAX_HASHES_PER_REQUEST / 2;
    let mut source = peers.source_with_chunks(chunk_heights - 1, chunk_heights);
    source.request(block::Height(0), block::Height(chunk_heights - 1));
    changed(&mut source).await;
    assert_resident(&source, 0, chunk_heights - 1);

    let calls = peers.drain_calls();
    let peer = calls.first().expect("the chunk was requested").peer;
    assert_eq!(
        calls,
        vec![
            Call {
                peer,
                start: 0,
                count: MAX_HASHES_PER_REQUEST
            },
            Call {
                peer,
                start: MAX_HASHES_PER_REQUEST,
                count: MAX_HASHES_PER_REQUEST
            },
            Call {
                peer,
                start: 2 * MAX_HASHES_PER_REQUEST,
                count: MAX_HASHES_PER_REQUEST / 2
            },
        ]
    );
}

/// Once every peer has failed, peer-set changes do not start a new round:
/// the peers already tried are asked again only after the retry interval,
/// which doubles after each failed round, while a newly connected peer is
/// asked at once.
#[tokio::test(start_paused = true)]
async fn failed_peers_are_retried_only_after_the_retry_interval() {
    let _init_guard = zebra_test::init();
    let mut peers = TestPeers::new();
    let liar = peers.peer(1, Behavior::Lying);
    peers.connect(vec![liar.clone()]);

    let mut source = peers.source(9);
    source.request(block::Height(0), block::Height(9));
    assert_eq!(peers.next_call().await.peer, 1);

    for interval in [CHUNK_RETRY_INTERVAL, 2 * CHUNK_RETRY_INTERVAL] {
        let round_start = tokio::time::Instant::now();

        // Peer-set churn while the round waits does not ask the liar again.
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            peers.connect(vec![liar.clone()]);
        }
        assert!(
            tokio::time::timeout(interval - Duration::from_secs(5), peers.next_call())
                .await
                .is_err(),
            "the liar is not asked again before the retry interval"
        );

        assert_eq!(
            peers.next_call().await.peer,
            1,
            "the next round asks the liar"
        );
        assert!(round_start.elapsed() >= interval);
    }

    // A peer that connects while the round waits is asked at once.
    tokio::time::sleep(Duration::from_secs(1)).await;
    peers.connect(vec![liar, peers.peer(2, Behavior::HonestThrough(u32::MAX))]);
    tokio::time::timeout(Duration::from_secs(1), source.changed())
        .await
        .expect("the new peer is asked without waiting for the round")
        .expect("the source has not failed");
    assert_resident(&source, 0, 9);
}

/// While a chunk is slow, its heights have no entries, so units there are
/// planned with checkpoint anchors. Known-hash units start once it arrives.
#[tokio::test(start_paused = true)]
async fn slow_chunks_fall_back_to_checkpoint_anchoring() {
    let _init_guard = zebra_test::init();
    let mut peers = TestPeers::new();
    let stalled = peers.peer(1, Behavior::Stalled);
    peers.connect(vec![stalled.clone()]);

    let mut source = peers.source(9);
    source.request(block::Height(0), block::Height(9));
    peers.next_call().await;

    let plan = |source: &KnownHashes<FakePeer>| {
        known_hash_unit(
            source,
            UnitId(0),
            block::Height(0),
            block::Height(9),
            FRONTIER_UNIT_BYTES,
        )
    };

    // `changed` stays pending while the only peer is stalled.
    assert!(
        tokio::time::timeout(Duration::from_secs(10), source.changed())
            .await
            .is_err(),
        "a stalled peer delivers nothing"
    );
    assert_eq!(
        plan(&source),
        None,
        "no known-hash unit while the chunk is slow"
    );

    let honest_peer = peers.peer(2, Behavior::HonestThrough(u32::MAX));
    peers.connect(vec![stalled, honest_peer]);
    changed(&mut source).await;

    let unit = plan(&source).expect("the chunk is resident");
    assert_eq!(unit.anchor, Anchor::KnownHash);
    assert_eq!((unit.start, unit.end), (block::Height(0), block::Height(9)));
    assert_eq!(unit.end_hash, honest(9).hash);
    assert_eq!(
        unit.est_bytes,
        (0..=9).map(|height| honest(height).size).sum::<u64>()
    );
}

/// At most [`MAX_RESIDENT_CHUNKS`] chunks are resident or fetched, all within
/// that many chunks of the requested start, and `release_below` frees them.
#[tokio::test(start_paused = true)]
async fn residency_is_bounded() {
    let _init_guard = zebra_test::init();
    let mut peers = TestPeers::new();
    peers.connect(vec![peers.peer(1, Behavior::HonestThrough(u32::MAX))]);

    let mut source = peers.source(99);
    source.request(block::Height(0), block::Height(99));
    for _ in 0..MAX_RESIDENT_CHUNKS {
        changed(&mut source).await;
    }
    assert_resident(&source, 0, 39);
    assert_eq!(source.entry(block::Height(40)), None);
    assert!(peers.drain_calls().iter().all(|call| call.start < 40));

    // Nothing more is fetched until the frontier moves.
    assert!(
        tokio::time::timeout(Duration::from_secs(60), source.changed())
            .await
            .is_err(),
        "nothing is fetched beyond the residency bound"
    );

    source.release_below(block::Height(20));
    assert_eq!(source.entry(block::Height(19)), None, "released");
    assert_resident(&source, 20, 39);

    source.request(block::Height(20), block::Height(99));
    for _ in 0..2 {
        changed(&mut source).await;
    }
    assert_resident(&source, 20, 59);
    assert_eq!(source.entry(block::Height(60)), None);

    // A request above the resident chunks evicts the chunks below its start.
    source.request(block::Height(80), block::Height(99));
    for _ in 0..2 {
        changed(&mut source).await;
    }
    assert_resident(&source, 80, 99);
    assert_eq!(source.resident.len(), MAX_RESIDENT_CHUNKS);
    assert_eq!(source.entry(block::Height(20)), None, "evicted");
}

/// `cumulative_txouts` sums the verified entries through any height, and
/// refuses heights above the list.
#[tokio::test(start_paused = true)]
async fn cumulative_txouts_sums_verified_entries() {
    let _init_guard = zebra_test::init();
    let peers = TestPeers::new();
    peers.connect(vec![
        peers.peer(1, Behavior::Lying),
        peers.peer(2, Behavior::HonestThrough(u32::MAX)),
    ]);

    let mut source = peers.source(64);
    let expected = |through: u32| {
        (0..=through)
            .map(|height| honest(height).txouts)
            .sum::<u64>()
    };

    for through in [0, 9, 10, 37, 64] {
        let total = tokio::time::timeout(
            TEST_TIMEOUT,
            source.cumulative_txouts(block::Height(through)),
        )
        .await
        .expect("finishes in time")
        .expect("the chunks verify");
        assert_eq!(total, expected(through), "through {through}");
    }
    assert!(source.resident.is_empty(), "nothing is kept resident");

    source
        .cumulative_txouts(block::Height(65))
        .await
        .expect_err("heights above the list are refused");
}

/// The source fails for good when the peer registry stops, and the engine
/// then continues with checkpoint anchors only.
#[tokio::test(start_paused = true)]
async fn source_fails_when_the_registry_stops() {
    let _init_guard = zebra_test::init();
    let peers = TestPeers::new();
    let mut source = peers.source(9);
    source.request(block::Height(0), block::Height(9));
    drop(peers);

    tokio::time::timeout(TEST_TIMEOUT, source.changed())
        .await
        .expect("finishes in time")
        .expect_err("the source fails without a registry");
    source
        .changed()
        .await
        .expect_err("the failure is permanent");
    assert_eq!(source.entry(block::Height(0)), None);
}

/// Resident entries for the unit-planning tests.
#[derive(Debug)]
struct Entries(BTreeMap<u32, SyncHashEntry>);

impl Entries {
    /// Entries of `size` bytes for `heights`.
    fn sized(heights: std::ops::RangeInclusive<u32>, size: u64) -> Self {
        Entries(
            heights
                .map(|height| {
                    (
                        height,
                        SyncHashEntry {
                            size,
                            ..honest(height)
                        },
                    )
                })
                .collect(),
        )
    }
}

impl HashSource for Entries {
    fn max_height(&self) -> block::Height {
        block::Height(*self.0.keys().last().expect("tests use some entries"))
    }

    fn entry(&self, height: block::Height) -> Option<SyncHashEntry> {
        self.0.get(&height.0).copied()
    }

    fn request(&mut self, _start: block::Height, _end: block::Height) {}

    fn changed(&mut self) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + '_>> {
        Box::pin(std::future::pending())
    }

    fn release_below(&mut self, _height: block::Height) {}
}

/// Units are cut at exactly their byte budget, at the limit, at the first
/// missing entry, or at the protocol's count bound.
#[test]
fn units_have_exact_sizes() {
    let _init_guard = zebra_test::init();
    const MIB: u64 = 1024 * 1024;

    // 1 MiB blocks: a frontier unit is 16 blocks, a deep unit 64.
    let entries = Entries::sized(100..=299, MIB);
    let frontier = known_hash_unit(
        &entries,
        UnitId(1),
        block::Height(100),
        block::Height(299),
        FRONTIER_UNIT_BYTES,
    )
    .expect("resident");
    assert_eq!(frontier.id, UnitId(1));
    assert_eq!(
        (frontier.start, frontier.end),
        (block::Height(100), block::Height(115))
    );
    assert_eq!(frontier.est_bytes, FRONTIER_UNIT_BYTES);
    assert_eq!(frontier.end_hash, honest(115).hash);
    assert_eq!(frontier.anchor, Anchor::KnownHash);
    assert_eq!(u64::from(frontier.max_bytes), DEEP_UNIT_BYTES);

    let deep = known_hash_unit(
        &entries,
        UnitId(2),
        block::Height(116),
        block::Height(299),
        DEEP_UNIT_BYTES,
    )
    .expect("resident");
    assert_eq!(deep.end, block::Height(179));
    assert_eq!(deep.est_bytes, DEEP_UNIT_BYTES);

    // The limit, and the first missing entry, end a unit early.
    let limited = known_hash_unit(
        &entries,
        UnitId(3),
        block::Height(290),
        block::Height(295),
        DEEP_UNIT_BYTES,
    )
    .expect("resident");
    assert_eq!(limited.end, block::Height(295));
    let short = known_hash_unit(
        &entries,
        UnitId(4),
        block::Height(290),
        block::Height(400),
        DEEP_UNIT_BYTES,
    )
    .expect("resident");
    assert_eq!(short.end, block::Height(299));

    // A missing start entry falls back to checkpoint anchors, and so does an empty range.
    let missing = known_hash_unit(
        &entries,
        UnitId(5),
        block::Height(300),
        block::Height(400),
        DEEP_UNIT_BYTES,
    );
    assert_eq!(missing, None);
    let empty = known_hash_unit(
        &entries,
        UnitId(6),
        block::Height(200),
        block::Height(199),
        DEEP_UNIT_BYTES,
    );
    assert_eq!(empty, None);

    // A block larger than the budget is still a unit.
    let large = Entries::sized(0..=1, 2 * FRONTIER_UNIT_BYTES);
    let unit = known_hash_unit(
        &large,
        UnitId(7),
        block::Height(0),
        block::Height(1),
        FRONTIER_UNIT_BYTES,
    )
    .expect("resident");
    assert_eq!((unit.start, unit.end), (block::Height(0), block::Height(0)));

    // Tiny blocks stop at the protocol's count bound.
    let tiny = Entries::sized(0..=MAX_UNIT_COUNT + 10, 1);
    let unit = known_hash_unit(
        &tiny,
        UnitId(8),
        block::Height(0),
        block::Height(MAX_UNIT_COUNT + 10),
        DEEP_UNIT_BYTES,
    )
    .expect("resident");
    assert_eq!(unit.end, block::Height(MAX_UNIT_COUNT - 1));
    assert_eq!(unit.est_bytes, u64::from(MAX_UNIT_COUNT));
}

/// `cumulative_txouts` waits for the chunks the source is already fetching
/// instead of fetching them again, and they stay resident.
#[tokio::test]
async fn cumulative_txouts_reuses_chunks_being_fetched() {
    let _init_guard = zebra_test::init();
    let mut peers = TestPeers::new();
    peers.connect(vec![peers.peer(1, Behavior::HonestThrough(u32::MAX))]);

    let mut source = peers.source(39);
    source.request(block::Height(0), block::Height(19));
    let total = tokio::time::timeout(TEST_TIMEOUT, source.cumulative_txouts(block::Height(29)))
        .await
        .expect("finishes in time")
        .expect("the chunks verify");
    assert_eq!(
        total,
        (0..=29).map(|height| honest(height).txouts).sum::<u64>()
    );

    let mut starts: Vec<u32> = peers.drain_calls().iter().map(|call| call.start).collect();
    starts.sort_unstable();
    assert_eq!(starts, vec![0, 10, 20], "each chunk is fetched once");
    assert_resident(&source, 0, 19);
}

/// Config pins are 64 hex digits each, and the field is optional.
#[test]
fn config_pins_parse_from_hex() {
    let _init_guard = zebra_test::init();

    let config: crate::components::sync::Config = toml::from_str(&format!(
        "[v2_known_hash_pins]\nmax_height = 99999\nchunk_hashes = [\"{}\", \"{}\"]\n",
        "00".repeat(32),
        "ab".repeat(32),
    ))
    .expect("valid pins parse");
    let pins = config.v2_known_hash_pins.expect("pins are configured");
    assert_eq!(pins.max_height, block::Height(99_999));
    assert_eq!(
        pins.chunk_hashes,
        vec![ChunkPin([0; 32]), ChunkPin([0xab; 32])]
    );

    let round_trip: KnownHashPins =
        toml::from_str(&toml::to_string(&pins).expect("pins serialize")).expect("pins parse");
    assert_eq!(round_trip, pins);

    let default: crate::components::sync::Config = toml::from_str("").expect("empty config");
    assert_eq!(default.v2_known_hash_pins, None);

    for bad in ["00", &"zz".repeat(32), &"00".repeat(33)] {
        toml::from_str::<KnownHashPins>(&format!("max_height = 0\nchunk_hashes = [\"{bad}\"]\n"))
            .expect_err("a pin must be 64 hex digits");
    }
}

/// Mainnet and the default Testnet refuse configured pins, which `start`
/// turns into a startup failure; Regtest takes consistent pins, and has no
/// list without them.
#[test]
fn known_hash_list_checks_configured_pins() {
    use zebra_chain::parameters::{testnet::RegtestParameters, Network};

    let _init_guard = zebra_test::init();

    let pins = |max_height: u32, chunks: usize| KnownHashPins {
        max_height: block::Height(max_height),
        chunk_hashes: vec![ChunkPin([7; 32]); chunks],
    };

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        assert!(
            matches!(
                known_hash_list(&network, Some(&pins(0, 1))),
                Err(KnownHashError::PinsNotConfigurable { .. })
            ),
            "{network} refuses configured pins"
        );
        assert_eq!(
            known_hash_list(&network, None),
            Ok(KnownHashListSpec::for_network(&network)),
            "{network} uses its compiled-in list"
        );
    }

    let regtest = Network::new_regtest(RegtestParameters::default());
    assert_eq!(known_hash_list(&regtest, None), Ok(None));
    assert!(
        matches!(
            known_hash_list(&regtest, Some(&pins(0, 2))),
            Err(KnownHashError::InconsistentSpec(_))
        ),
        "two chunks for one height are inconsistent"
    );
    let list = known_hash_list(&regtest, Some(&pins(0, 1)))
        .expect("consistent pins at or below the final checkpoint")
        .expect("configured pins make a list");
    assert_eq!(list.max_height, block::Height(0));
    assert_eq!(list.chunk_hashes.as_ref(), &[[7; 32]]);
}
