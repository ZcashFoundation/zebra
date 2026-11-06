//! Tests for spentness-hint acquisition, verification and serving.
//!
//! The scripted peers count requests, penalties and violations with atomics,
//! so the tests need no locks.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use bytes::Bytes;
use tokio::time::Instant;

use super::*;

/// What a scripted peer does with every `get-object` request.
#[derive(Clone, Debug)]
enum Behavior {
    /// Serves its object, at most `piece` bytes per answer.
    Serve { piece: usize },
    /// Answers `not found`.
    NotFound,
    /// Answers `not found` to its first `n` requests, then serves.
    NotFoundFirst(usize),
    /// Serves its first piece, then claims a different size.
    ChangeSize { piece: usize },
    /// Claims a size one byte longer than its object.
    WrongSize,
    /// Never answers.
    Stall,
    /// Waits `delay` before every answer, then serves at most `piece` bytes.
    Slow { delay: Duration, piece: usize },
    /// Claims the object's size, but sends no data.
    Dribble,
}

#[derive(Debug)]
struct Inner {
    label: &'static str,
    serves: bool,
    behavior: Behavior,
    object: Arc<[u8]>,
    requests: AtomicUsize,
    penalty: AtomicU32,
    violations: AtomicUsize,
}

/// A scripted peer.
#[derive(Clone, Debug)]
struct FakePeer(Arc<Inner>);

impl FakePeer {
    fn new(label: &'static str, behavior: Behavior, object: &[u8]) -> Self {
        Self::with_service(label, behavior, object, true)
    }

    fn not_serving(label: &'static str, object: &[u8]) -> Self {
        Self::with_service(label, Behavior::Serve { piece: usize::MAX }, object, false)
    }

    fn with_service(label: &'static str, behavior: Behavior, object: &[u8], serves: bool) -> Self {
        FakePeer(Arc::new(Inner {
            label,
            serves,
            behavior,
            object: object.into(),
            requests: AtomicUsize::new(0),
            penalty: AtomicU32::new(0),
            violations: AtomicUsize::new(0),
        }))
    }

    fn requests(&self) -> usize {
        self.0.requests.load(Ordering::SeqCst)
    }

    fn penalty(&self) -> u32 {
        self.0.penalty.load(Ordering::SeqCst)
    }

    fn violations(&self) -> usize {
        self.0.violations.load(Ordering::SeqCst)
    }

    fn size(&self) -> u64 {
        u64::try_from(self.0.object.len()).expect("test objects fit u64")
    }

    fn answer(&self, offset: u64, length: u32, piece: usize) -> ObjectChunk {
        let object = &self.0.object;
        let start = usize::try_from(offset)
            .expect("test offsets fit usize")
            .min(object.len());
        let len = piece
            .min(usize::try_from(length).expect("u32 fits usize"))
            .min(object.len() - start);
        ObjectChunk::Found {
            size: self.size(),
            data: Bytes::copy_from_slice(&object[start..start + len]),
        }
    }
}

impl ArtifactPeer for FakePeer {
    fn serves_artifacts(&self) -> bool {
        self.0.serves
    }

    fn label(&self) -> String {
        self.0.label.to_string()
    }

    fn get_object(
        &self,
        _hash: ObjectHash,
        offset: u64,
        length: u32,
    ) -> impl Future<Output = Result<ObjectChunk, SyncPeerError>> + Send {
        let request = self.0.requests.fetch_add(1, Ordering::SeqCst);
        let peer = self.clone();

        async move {
            match peer.0.behavior.clone() {
                Behavior::Serve { piece } => Ok(peer.answer(offset, length, piece)),
                Behavior::NotFound => Ok(ObjectChunk::NotFound),
                Behavior::NotFoundFirst(n) if request < n => Ok(ObjectChunk::NotFound),
                Behavior::NotFoundFirst(_) => Ok(peer.answer(offset, length, usize::MAX)),
                Behavior::ChangeSize { piece } if request == 0 => {
                    Ok(peer.answer(offset, length, piece))
                }
                Behavior::ChangeSize { .. } => Ok(ObjectChunk::Found {
                    size: peer.size() + 1,
                    data: Bytes::new(),
                }),
                Behavior::WrongSize => Ok(ObjectChunk::Found {
                    size: peer.size() + 1,
                    data: Bytes::from_static(&[0; 16]),
                }),
                Behavior::Stall => std::future::pending().await,
                Behavior::Slow { delay, piece } => {
                    tokio::time::sleep(delay).await;
                    Ok(peer.answer(offset, length, piece))
                }
                Behavior::Dribble => Ok(ObjectChunk::Found {
                    size: peer.size(),
                    data: Bytes::new(),
                }),
            }
        }
    }

    fn penalize(&self, points: u32, _reason: String) -> impl Future<Output = ()> + Send {
        self.0.penalty.fetch_add(points, Ordering::SeqCst);
        std::future::ready(())
    }

    fn report_violation(&self, _reason: String) {
        self.0.violations.fetch_add(1, Ordering::SeqCst);
    }
}

/// The length of the pinned object, as `artifact_len` would compute it.
const OBJECT_LEN: u64 = 5000;

/// The pinned object: [`OBJECT_LEN`] bytes, more than one short piece.
fn pinned_object() -> Vec<u8> {
    (0..u32::try_from(OBJECT_LEN).expect("the test length fits u32"))
        .map(|i| u8::try_from(i % 251).expect("the remainder fits u8"))
        .collect()
}

/// Well-formed bytes that are not the pinned object.
fn corrupted_object() -> Vec<u8> {
    let mut object = pinned_object();
    object[4321] ^= 0x01;
    object
}

fn peers(list: &[FakePeer]) -> watch::Receiver<Arc<[FakePeer]>> {
    watch::channel(Arc::from(list)).1
}

/// The files in `dir`, by name.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| {
                    entry
                        .expect("test directory entries are readable")
                        .file_name()
                        .into_string()
                        .expect("test file names are UTF-8")
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A peer that serves the whole object in short pieces is asked from the end
/// of what it delivered, and the assembled object is accepted.
#[tokio::test]
async fn assembles_short_pieces_from_one_peer() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let peer = FakePeer::new("short", Behavior::Serve { piece: 1000 }, &object);

    let bytes = fetch_whole(&peer, hash, OBJECT_LEN)
        .await
        .expect("the object assembles");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(peer.requests(), 5);
    assert_eq!(peer.penalty(), 0);
}

/// A corrupted artifact from a whole-object peer costs it 100 points and is
/// never returned or written: the next peer's correct artifact is used.
#[tokio::test]
async fn corrupted_artifact_is_penalized_and_the_next_peer_is_used() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let bad = FakePeer::new("bad", Behavior::Serve { piece: 1000 }, &corrupted_object());
    let good = FakePeer::new("good", Behavior::Serve { piece: usize::MAX }, &object);
    let dir = tempfile::tempdir().expect("a temporary directory");

    let (bytes, source) = load_or_fetch(
        Mode::Auto,
        hash,
        OBJECT_LEN,
        peers(&[bad.clone(), good.clone()]),
        dir.path(),
    )
    .await
    .expect("the good peer serves the artifact");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(source, Source::Fetched);
    assert_eq!(bad.penalty(), MISMATCH_PENALTY);
    assert_eq!(bad.violations(), 0);
    assert_eq!(good.penalty(), 0);
    // Saving waits for verification, which is the caller's next step.
    assert!(files(dir.path()).is_empty());
}

/// When every peer serves a corrupted artifact, `Auto` gives up with
/// `Invalid`, nothing is written, and each peer is penalized once per round.
#[tokio::test(start_paused = true)]
async fn only_corrupted_artifacts_are_rejected_with_no_writes() {
    let _init_guard = zebra_test::init();

    let hash = ObjectHash::of(&pinned_object());
    let bad = FakePeer::new(
        "bad",
        Behavior::Serve { piece: usize::MAX },
        &corrupted_object(),
    );
    let dir = tempfile::tempdir().expect("a temporary directory");

    let result = load_or_fetch(
        Mode::Auto,
        hash,
        OBJECT_LEN,
        peers(std::slice::from_ref(&bad)),
        dir.path(),
    )
    .await;

    assert!(matches!(result, Err(HintsError::Invalid(_))), "{result:?}");
    assert_eq!(
        bad.penalty(),
        MISMATCH_PENALTY * u32::try_from(AUTO_ROUNDS).expect("few rounds")
    );
    assert!(files(dir.path()).is_empty());
}

/// When no peer has the artifact, `Auto` gives up after its rounds with
/// `NotFound`, so the caller syncs unhinted.
#[tokio::test(start_paused = true)]
async fn not_found_falls_back() {
    let _init_guard = zebra_test::init();

    let hash = ObjectHash::of(&pinned_object());
    let a = FakePeer::new("a", Behavior::NotFound, &[]);
    let b = FakePeer::new("b", Behavior::NotFound, &[]);
    let started = Instant::now();

    let result =
        fetch_from_peers(Mode::Auto, hash, OBJECT_LEN, peers(&[a.clone(), b.clone()])).await;

    assert!(matches!(result, Err(HintsError::NotFound)), "{result:?}");
    assert_eq!(a.requests(), AUTO_ROUNDS);
    assert_eq!(b.requests(), AUTO_ROUNDS);
    assert_eq!(a.penalty() + b.penalty(), 0);
    assert!(started.elapsed() < AUTO_TIMEOUT);
}

/// With no candidate peers at all, `Auto` gives up at its deadline.
#[tokio::test(start_paused = true)]
async fn no_peers_auto_times_out() {
    let _init_guard = zebra_test::init();

    let hash = ObjectHash::of(&pinned_object());
    let (_sender, receiver) = watch::channel(Arc::<[FakePeer]>::from([]));
    let started = Instant::now();

    let result = fetch_from_peers(Mode::Auto, hash, OBJECT_LEN, receiver).await;

    assert!(matches!(result, Err(HintsError::NotFound)), "{result:?}");
    assert_eq!(started.elapsed(), AUTO_TIMEOUT);
}

/// `Auto` bounds the whole acquisition, slow peers included, by its deadline.
#[tokio::test(start_paused = true)]
async fn slow_peers_auto_times_out() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let stalled: Vec<FakePeer> = (0..8)
        .map(|_| FakePeer::new("stalled", Behavior::Stall, &object))
        .collect();
    let started = Instant::now();

    let result = fetch_from_peers(Mode::Auto, hash, OBJECT_LEN, peers(&stalled)).await;

    assert!(matches!(result, Err(HintsError::NotFound)), "{result:?}");
    assert_eq!(started.elapsed(), AUTO_TIMEOUT);
}

/// Peers that connect after acquisition started are used.
#[tokio::test(start_paused = true)]
async fn peers_that_connect_later_are_used() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let (sender, receiver) = watch::channel(Arc::<[FakePeer]>::from([]));
    let good = FakePeer::new("good", Behavior::Serve { piece: usize::MAX }, &object);

    let fetch = tokio::spawn(fetch_from_peers(Mode::Auto, hash, OBJECT_LEN, receiver));
    tokio::time::sleep(Duration::from_secs(30)).await;
    sender.send_replace(Arc::from([good.clone()]));

    let bytes = fetch
        .await
        .expect("the fetch task does not panic")
        .expect("the late peer serves the artifact");
    assert_eq!(&*bytes, &object[..]);
}

/// Peers without `NODE_SYNC_ARTIFACTS` are never asked.
#[tokio::test(start_paused = true)]
async fn peers_without_the_service_are_not_asked() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let other = FakePeer::not_serving("other", &object);

    let result = fetch_from_peers(
        Mode::Auto,
        hash,
        OBJECT_LEN,
        peers(std::slice::from_ref(&other)),
    )
    .await;

    assert!(matches!(result, Err(HintsError::NotFound)), "{result:?}");
    assert_eq!(other.requests(), 0);
}

/// `Require` keeps retrying after `Auto` would have given up.
#[tokio::test(start_paused = true)]
async fn require_retries_until_the_artifact_is_served() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let rounds = AUTO_ROUNDS + 2;
    let late = FakePeer::new("late", Behavior::NotFoundFirst(rounds), &object);

    let bytes = fetch_from_peers(
        Mode::Require,
        hash,
        OBJECT_LEN,
        peers(std::slice::from_ref(&late)),
    )
    .await
    .expect("the peer serves the artifact eventually");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(late.requests(), rounds + 1);
}

/// A peer whose `size` changes between answers is reported as a protocol
/// violation and not penalized; a peer claiming a size other than the
/// expected length is dropped after one request, before its data is kept,
/// with no blame.
#[tokio::test]
async fn inconsistent_and_wrong_sizes_are_not_penalized() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);

    let changing = FakePeer::new("changing", Behavior::ChangeSize { piece: 1000 }, &object);
    let result = fetch_whole(&changing, hash, OBJECT_LEN).await;
    assert!(
        matches!(result, Err(PeerFailure::Violation(_))),
        "{result:?}"
    );
    assert_eq!(changing.violations(), 1);
    assert_eq!(changing.penalty(), 0);

    let wrong = FakePeer::new("wrong", Behavior::WrongSize, &object);
    let result = fetch_whole(&wrong, hash, OBJECT_LEN).await;
    assert!(
        matches!(result, Err(PeerFailure::Size(size)) if size == OBJECT_LEN + 1),
        "{result:?}"
    );
    assert_eq!(wrong.requests(), 1);
    assert_eq!(wrong.violations(), 0);
    assert_eq!(wrong.penalty(), 0);
}

/// A peer that never answers times out, and a peer that sends no data is
/// dropped after a bounded number of requests; neither is penalized.
#[tokio::test(start_paused = true)]
async fn stalled_and_dribbling_peers_are_dropped() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);

    let stalled = FakePeer::new("stalled", Behavior::Stall, &object);
    let result = fetch_whole(&stalled, hash, OBJECT_LEN).await;
    assert!(
        matches!(result, Err(PeerFailure::Unavailable(_))),
        "{result:?}"
    );

    let dribbling = FakePeer::new("dribbling", Behavior::Dribble, &object);
    let result = fetch_whole(&dribbling, hash, OBJECT_LEN).await;
    assert!(
        matches!(result, Err(PeerFailure::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(
        u64::try_from(dribbling.requests()).expect("few requests"),
        1 + EXTRA_REQUESTS
    );

    assert_eq!(stalled.penalty() + dribbling.penalty(), 0);
}

/// A peer that answers every request with one byte just inside the
/// per-request timeout is dropped after its first answer, which is below the
/// minimum rate, so `Auto` gets the artifact from the next peer well within
/// its deadline.
#[tokio::test(start_paused = true)]
async fn a_slow_first_peer_does_not_hold_acquisition() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let slow = FakePeer::new(
        "slow",
        Behavior::Slow {
            delay: PIECE_TIMEOUT - Duration::from_secs(1),
            piece: 1,
        },
        &object,
    );
    let good = FakePeer::new("good", Behavior::Serve { piece: usize::MAX }, &object);
    let started = Instant::now();

    let bytes = fetch_from_peers(
        Mode::Auto,
        hash,
        OBJECT_LEN,
        peers(&[slow.clone(), good.clone()]),
    )
    .await
    .expect("the second peer serves the artifact");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(started.elapsed(), PIECE_TIMEOUT - Duration::from_secs(1));
    assert!(started.elapsed() < AUTO_TIMEOUT);
    assert_eq!(slow.requests(), 1);
    assert_eq!(slow.penalty(), 0);
    assert_eq!(good.requests(), 1);
}

/// A peer that keeps up the minimum rate with short answers is kept, however
/// many it needs.
#[tokio::test(start_paused = true)]
async fn a_peer_at_the_minimum_rate_is_kept() {
    let _init_guard = zebra_test::init();

    let len: u64 = 10 * MIN_RATE;
    let object: Vec<u8> = (0..len).map(|position| position.to_le_bytes()[0]).collect();
    let hash = ObjectHash::of(&object);
    let paced = FakePeer::new(
        "paced",
        Behavior::Slow {
            delay: Duration::from_secs(1),
            piece: usize::try_from(MIN_RATE).expect("MIN_RATE fits usize"),
        },
        &object,
    );

    let bytes = fetch_whole(&paced, hash, len)
        .await
        .expect("a peer at the minimum rate is kept");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(paced.requests(), 10);
}

/// The minimum rate applies after its grace period, to the bytes delivered
/// since the first request.
#[test]
fn min_rate_has_a_grace_period() {
    assert!(!below_min_rate(0, RATE_GRACE));
    assert!(below_min_rate(0, RATE_GRACE + Duration::from_secs(1)));
    assert!(!below_min_rate(
        MIN_RATE,
        RATE_GRACE + Duration::from_secs(1)
    ));
    assert!(below_min_rate(
        MIN_RATE - 1,
        RATE_GRACE + Duration::from_secs(1)
    ));
}

/// Peers that failed in an earlier round are asked after the peers that
/// connected since, so they do not hold acquisition round after round.
#[tokio::test(start_paused = true)]
async fn peers_that_failed_are_asked_last() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let first = FakePeer::new("first", Behavior::Stall, &object);
    let second = FakePeer::new("second", Behavior::Stall, &object);
    let fresh = FakePeer::new("fresh", Behavior::Serve { piece: usize::MAX }, &object);
    let (sender, receiver) =
        watch::channel(Arc::<[FakePeer]>::from([first.clone(), second.clone()]));
    let started = Instant::now();

    let fetch = tokio::spawn(fetch_from_peers(Mode::Require, hash, OBJECT_LEN, receiver));
    // During the wait after the first round. The second round's rotation
    // alone would ask `second` first.
    tokio::time::sleep(2 * PIECE_TIMEOUT + Duration::from_secs(1)).await;
    sender.send_replace(Arc::from([first.clone(), second.clone(), fresh.clone()]));

    let bytes = fetch
        .await
        .expect("the fetch task does not panic")
        .expect("the fresh peer serves the artifact");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(started.elapsed(), 2 * PIECE_TIMEOUT + RETRY_INTERVAL);
    assert_eq!(first.requests(), 1);
    assert_eq!(second.requests(), 1);
    assert_eq!(fresh.requests(), 1);
}

/// Each round starts at a different peer, so a peer that failed first in one
/// round is not asked first in the next.
#[tokio::test(start_paused = true)]
async fn rounds_start_at_different_peers() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let slow = FakePeer::new(
        "slow",
        Behavior::Slow {
            delay: PIECE_TIMEOUT - Duration::from_secs(1),
            piece: 1,
        },
        &object,
    );
    let late = FakePeer::new("late", Behavior::NotFoundFirst(1), &object);
    let started = Instant::now();

    let bytes = fetch_from_peers(
        Mode::Require,
        hash,
        OBJECT_LEN,
        peers(&[slow.clone(), late.clone()]),
    )
    .await
    .expect("the second peer serves the artifact in the second round");

    assert_eq!(&*bytes, &object[..]);
    // Only the first round waited on the slow peer.
    assert_eq!(slow.requests(), 1);
    assert_eq!(late.requests(), 2);
    assert_eq!(
        started.elapsed(),
        PIECE_TIMEOUT - Duration::from_secs(1) + RETRY_INTERVAL
    );
}

/// Each peer's whole-object deadline allows [`MIN_RATE`], and never less
/// than one request's timeout.
#[test]
fn whole_object_deadline_follows_the_length() {
    assert_eq!(whole_object_timeout(OBJECT_LEN), PIECE_TIMEOUT);
    assert_eq!(
        whole_object_timeout(20 * 1024 * 1024),
        Duration::from_secs(80)
    );
}

/// A saved artifact is loaded without asking any peer.
#[tokio::test]
async fn a_saved_artifact_is_loaded_without_peers() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let dir = tempfile::tempdir().expect("a temporary directory");
    save_artifact(dir.path(), hash, object.clone().into())
        .await
        .expect("the artifact saves");
    let peer = FakePeer::new("peer", Behavior::Serve { piece: usize::MAX }, &object);

    let (bytes, source) = load_or_fetch(
        Mode::Auto,
        hash,
        OBJECT_LEN,
        peers(std::slice::from_ref(&peer)),
        dir.path(),
    )
    .await
    .expect("the saved artifact loads");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(source, Source::Local);
    assert_eq!(peer.requests(), 0);
    assert_eq!(files(dir.path()), vec![hash.to_string()]);
}

/// A saved file that does not hash to the pin is deleted and fetched again;
/// stale artifacts of other pins and partial files are deleted; other files
/// are kept.
#[tokio::test]
async fn corrupted_and_stale_files_are_deleted() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let stale = ObjectHash::of(b"an artifact of an earlier pin");
    let dir = tempfile::tempdir().expect("a temporary directory");

    std::fs::write(artifact_path(dir.path(), hash), corrupted_object()).expect("writes");
    assert_eq!(
        load_saved_artifact(dir.path(), hash, Some(OBJECT_LEN + 1)).expect("loading succeeds"),
        None,
        "a file of another length is not loaded",
    );
    assert!(files(dir.path()).is_empty(), "and is deleted");
    std::fs::write(artifact_path(dir.path(), hash), corrupted_object()).expect("writes");
    std::fs::write(artifact_path(dir.path(), stale), b"stale").expect("writes");
    std::fs::write(dir.path().join(format!("{stale}{TEMP_SUFFIX}")), b"partial").expect("writes");
    std::fs::write(dir.path().join(format!("{hash}{TEMP_SUFFIX}")), b"partial").expect("writes");
    std::fs::write(dir.path().join("README"), b"not ours").expect("writes");

    let peer = FakePeer::new("peer", Behavior::Serve { piece: usize::MAX }, &object);
    let (bytes, source) = load_or_fetch(
        Mode::Auto,
        hash,
        OBJECT_LEN,
        peers(std::slice::from_ref(&peer)),
        dir.path(),
    )
    .await
    .expect("the peer serves the artifact");

    assert_eq!(&*bytes, &object[..]);
    assert_eq!(source, Source::Fetched);
    assert_eq!(peer.requests(), 1);
    assert_eq!(files(dir.path()), vec!["README".to_string()]);
}

/// Serving writes a missing or truncated file, and adds its entry to the
/// published index without dropping the other entries.
#[test]
fn serving_registers_the_artifact_file() {
    let _init_guard = zebra_test::init();

    let object = pinned_object();
    let hash = ObjectHash::of(&object);
    let dir = tempfile::tempdir().expect("a temporary directory");

    let other = ObjectHash::of(b"another artifact");
    let other_entry = ObjectEntry {
        path: dir.path().join(other.to_string()),
        size: 16,
    };
    let (sender, receiver) =
        watch::channel(Arc::new(HashMap::from([(other, other_entry.clone())])));

    // A truncated file is rewritten.
    std::fs::write(artifact_path(dir.path(), hash), &object[..10]).expect("writes");

    let entry = store_for_serving(dir.path(), hash, &object).expect("the artifact is stored");
    register_object(&sender, hash, entry.clone());

    assert_eq!(entry.path, artifact_path(dir.path(), hash));
    assert_eq!(Ok(entry.size), u64::try_from(object.len()));
    assert_eq!(std::fs::read(&entry.path).expect("reads"), object);

    let index = receiver.borrow();
    assert_eq!(index.get(&hash), Some(&entry));
    assert_eq!(index.get(&other), Some(&other_entry));
}

/// The bit count must equal the known-hash `txouts` sum.
#[test]
fn output_count_must_match_the_known_hash_sum() {
    assert!(check_output_count(7, 7).is_ok());
    assert!(matches!(
        check_output_count(7, 8),
        Err(HintsError::OutputCount {
            bits: 7,
            outputs: 8
        })
    ));
}

/// `Off` does nothing, even with no peers and no pin.
#[tokio::test]
async fn off_does_nothing() {
    let _init_guard = zebra_test::init();

    let (_sender, receiver) = watch::channel(Arc::<[SyncPeer]>::from([]));
    let dir = tempfile::tempdir().expect("a temporary directory");

    let result = acquire_spentness_hints(
        Network::Mainnet,
        Mode::Off,
        receiver,
        0,
        dir.path().to_owned(),
    )
    .await;

    assert!(matches!(result, Ok(None)), "{result:?}");
    assert!(files(dir.path()).is_empty());
}

/// End to end with real artifacts: the pinned artifact verifies, and a bit
/// count that differs from the known-hash sum is rejected.
#[test]
fn pinned_artifact_verifies_and_output_count_is_checked() {
    use zebra_chain::{block, parameters::spentness_hints::ArtifactEncoder};

    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let genesis = network.genesis_hash();
    let height = block::Height(10);
    let terminal = block::Hash([0x42; 32]);

    let mut encoder =
        ArtifactEncoder::new(genesis, height, terminal, 3).expect("a valid artifact header");
    // The genesis output (ordinal 0) is never retained: Zebra writes no genesis UTXOs.
    for retained in [false, true, false] {
        encoder.push(retained).expect("three bits fit");
    }
    let bytes: Arc<[u8]> = encoder.finish().expect("three bits were pushed").into();

    let pin = MaxCheckpoint {
        height,
        hash: terminal,
        spentness_hash: ObjectHash::of(&bytes).0,
    };

    let verified =
        verify_pinned(&network, &pin, 3, bytes.clone()).expect("the pinned artifact verifies");
    assert_eq!(verified.header().output_count, 3);

    assert!(matches!(
        verify_pinned(&network, &pin, 4, bytes),
        Err(HintsError::OutputCount {
            bits: 3,
            outputs: 4
        })
    ));
}
