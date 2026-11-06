//! Tests of serving `get-object` requests from the object index.
//!
//! These tests call [`serve_object`] on accepted loopback streams directly, as the request stream
//! task does after decoding a request, and read the response as the requesting peer would.

use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant as StdInstant},
};

use futures::FutureExt;
use metrics::{
    atomics::AtomicU64, Counter, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString,
    Unit,
};
use quinn::Connection;
use tokio::{
    sync::{watch, Semaphore},
    time::Instant,
};

use zebra_chain::{
    parameters::Network,
    serialization::{CompactSize64, ZcashDeserialize},
};

use crate::{
    peer::v2::{
        connection::{
            inbound::{
                record_sent_object_bytes, request_permit, serve_object, spawn_object_disk,
                unreadable, ObjectServing, Outcome, ResponseSend, MAX_OBJECT_STREAMS_GLOBAL,
                MAX_OBJECT_STREAMS_PER_PEER, OBJECT_EARLY_FIN_MARGIN, OBJECT_READ_CHUNK,
                OBJECT_WRITE_STALL,
            },
            refuse_stream,
        },
        tests::{connected_pair, connected_pair_with},
    },
    protocol::v2::{
        constants::{MAX_OBJECT_OFFSET, MAX_OBJECT_REQUEST_LEN},
        object::{ObjectEntry, ObjectHash, ObjectIndex, ObjectSource},
        request::GetObject,
        response::{RESULT_FOUND, RESULT_NOT_FOUND},
        types::{ErrorCode, StreamType},
    },
};

/// Every loopback exchange finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The request deadline of a response that has all the time it needs.
const AMPLE: Duration = Duration::from_secs(30);

/// How early a timer may fire.
const TIMER_SLACK: Duration = Duration::from_millis(250);

/// What the requesting peer sees of a `get-object` response.
#[derive(Debug, Eq, PartialEq)]
enum Served {
    /// The stream was reset with this application error code.
    Reset(u64),

    /// `result` is not found, and the stream finished.
    NotFound,

    /// `result` is found, with the object's size and the delivered bytes, and
    /// the stream finished.
    Found { size: u64, data: Vec<u8> },
}

impl Served {
    /// A refusal: the stream reset with `REFUSED`.
    const REFUSED: Served = Served::Reset(ErrorCode::Refused.code());

    /// A probe answer: the size alone.
    fn probe(size: u64) -> Served {
        Served::Found {
            size,
            data: Vec::new(),
        }
    }
}

/// When the two sides of a [`serve`] exchange acted, measured from the node
/// starting to serve the request.
#[derive(Debug)]
struct Timing {
    /// When the node finished or refused the response.
    node_finished: Duration,

    /// When the peer started reading the response.
    peer_read_from: Duration,
}

/// Serves `request` on a stream `peer` opens to `node`, as the request stream
/// task does once it has decoded the request, with `deadline` as the
/// request's deadline. The peer starts reading the response after
/// `read_after`.
///
/// Returns what the peer saw, and when each side acted.
async fn serve(
    peer: &Connection,
    node: &Connection,
    request: GetObject,
    objects: &ObjectServing,
    deadline: Instant,
    read_after: Duration,
) -> (Served, Timing) {
    let (mut peer_send, mut peer_recv) = peer.open_bi().await.expect("opens");
    // The node only sees the stream once it carries data: the type byte stands in for the
    // request the connection task would have read.
    peer_send
        .write_all(&[StreamType::GetObject.code()])
        .await
        .expect("writes");
    peer_send.finish().expect("finishes");
    let (send, mut recv) = node.accept_bi().await.expect("accepts");

    let started = StdInstant::now();
    let node_side = async {
        let mut response = ResponseSend::new(send);
        match serve_object(&mut response, request, objects, deadline).await {
            Ok(()) => {
                let _ = response.send.finish();
            }
            Err(Outcome::Refuse(code)) => refuse_stream(Some(&mut response.send), &mut recv, code),
            Err(Outcome::Transport) => {}
            Err(Outcome::Violation { reason, code }) => {
                panic!("unexpected violation {code:?}: {reason}")
            }
            Err(Outcome::Close(_)) => panic!("unexpected connection close"),
        }
        started.elapsed()
    };
    let peer_side = async {
        tokio::time::sleep(read_after).await;
        let read_from = started.elapsed();
        // Objects in these tests are at most a few MiB.
        (read_from, peer_recv.read_to_end(64 << 20).await)
    };
    let (node_finished, (peer_read_from, read)) = tokio::join!(node_side, peer_side);
    let timing = Timing {
        node_finished,
        peer_read_from,
    };

    let served = match read {
        Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))) => {
            Served::Reset(code.into_inner())
        }
        Err(other) => panic!("unexpected read error {other:?}"),
        Ok(bytes) => match bytes.split_first() {
            Some((&RESULT_NOT_FOUND, rest)) => {
                assert!(rest.is_empty(), "nothing follows a not-found result");
                Served::NotFound
            }
            Some((&RESULT_FOUND, mut rest)) => {
                let size = CompactSize64::zcash_deserialize(&mut rest).expect("a canonical size");
                Served::Found {
                    size: size.into(),
                    data: rest.to_vec(),
                }
            }
            other => panic!("unexpected response start {other:?}"),
        },
    };
    (served, timing)
}

/// A request for `length` bytes of `hash` from `offset`.
fn request(hash: ObjectHash, offset: u64, length: u32) -> GetObject {
    GetObject {
        hash,
        offset,
        length,
    }
}

/// The serving params for `index`, with fresh permits, and the sender that
/// publishes the index.
fn serving(
    index: ObjectIndex,
    advertised: bool,
) -> (watch::Sender<Arc<ObjectIndex>>, ObjectServing) {
    let (sender, source) = ObjectSource::channel(Arc::new(index));
    let objects = ObjectServing {
        source: Some(source),
        advertised,
        peer_permits: Arc::new(Semaphore::new(MAX_OBJECT_STREAMS_PER_PEER)),
        global_permits: Arc::new(Semaphore::new(MAX_OBJECT_STREAMS_GLOBAL)),
    };
    (sender, objects)
}

/// Writes an object of `len` bytes with a position-dependent pattern to a
/// file in `dir`, and returns its hash, its index entry and its contents.
fn write_object(dir: &Path, len: usize) -> (ObjectHash, ObjectEntry, Vec<u8>) {
    let data: Vec<u8> = (0..len)
        .map(|position| {
            // Only the low byte of the mixed position is kept.
            (position.wrapping_mul(0x9E37_79B9) >> 13) as u8
        })
        .collect();
    let hash = ObjectHash::of(&data);
    let path = dir.join(hash.to_string());
    std::fs::write(&path, &data).expect("the temporary directory is writable");
    let entry = ObjectEntry {
        path,
        size: u64::try_from(len).expect("a test object's length fits in a u64"),
    };
    (hash, entry, data)
}

/// The size of an object that fits in a `u64`.
fn size_of(data: &[u8]) -> u64 {
    u64::try_from(data.len()).expect("a test object's length fits in a u64")
}

/// Asserts that every permit of `objects` is free again.
fn assert_permits_free(objects: &ObjectServing) {
    assert_eq!(
        objects.peer_permits.available_permits(),
        MAX_OBJECT_STREAMS_PER_PEER
    );
    assert_eq!(
        objects.global_permits.available_permits(),
        MAX_OBJECT_STREAMS_GLOBAL
    );
}

/// A timed-out blocking disk operation keeps both object permits until its closure actually exits.
#[tokio::test]
async fn timed_out_object_disk_operations_hold_their_permits() {
    let peer = Arc::new(Semaphore::new(1));
    let global = Arc::new(Semaphore::new(1));
    let permits = Arc::new((
        peer.clone()
            .try_acquire_owned()
            .expect("a free peer permit"),
        global
            .clone()
            .try_acquire_owned()
            .expect("a free global permit"),
    ));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut disk = spawn_object_disk(&permits, move || {
        started_tx
            .send(())
            .expect("the test waits for the disk operation to start");
        release_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the test releases the disk operation");
    });
    started_rx.await.expect("the disk operation starts");
    tokio::time::timeout(Duration::from_millis(50), &mut disk)
        .await
        .expect_err("the blocked disk operation times out");
    drop(disk);
    drop(permits);

    assert_eq!(peer.available_permits(), 0);
    assert_eq!(global.available_permits(), 0);
    release_tx.send(()).expect("the blocking closure is alive");
    tokio::time::timeout(Duration::from_secs(1), async {
        while peer.available_permits() != 1 || global.available_permits() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the finished disk operation releases both permits");
}

#[derive(Default)]
struct ObjectMetricsRecorder {
    sent: Arc<AtomicU64>,
    inbound: Arc<AtomicU64>,
}

impl Recorder for ObjectMetricsRecorder {
    fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
        match key.name() {
            "zebra.net.out.objects.bytes" => Counter::from_arc(self.sent.clone()),
            "zebra.net.in.objects.bytes" => Counter::from_arc(self.inbound.clone()),
            _ => Counter::noop(),
        }
    }

    fn register_gauge(&self, _key: &Key, _metadata: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, _key: &Key, _metadata: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

/// Object payload bytes are counted in the node's outbound direction.
#[test]
fn sent_object_bytes_use_the_outbound_metric() {
    let recorder = ObjectMetricsRecorder::default();
    metrics::with_local_recorder(&recorder, || record_sent_object_bytes(4096));
    assert_eq!(recorder.sent.load(Ordering::Relaxed), 4096);
    assert_eq!(recorder.inbound.load(Ordering::Relaxed), 0);
}

/// A `get-object` request is refused with `REFUSED` unless this node
/// advertises `NODE_SYNC_ARTIFACTS` and holds an object index, however the
/// index answers it; the same request is served once both hold.
#[tokio::test]
async fn object_requests_are_refused_without_the_artifact_service() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (peer, node, _client_ep, _server_ep) = connected_pair(&network).await;
        let dir = tempfile::tempdir().expect("a temporary directory");
        let (hash, entry, data) = write_object(dir.path(), 1024);
        let index: ObjectIndex = [(hash, entry)].into_iter().collect();
        let deadline = || Instant::now() + AMPLE;

        // The index holds the object, but the bit is not advertised.
        let (_index_tx, unadvertised) = serving(index.clone(), false);
        for probe in [request(hash, 0, 1024), request(hash, 0, 0)] {
            let (served, _) = serve(
                &peer,
                &node,
                probe,
                &unadvertised,
                deadline(),
                Duration::ZERO,
            )
            .await;
            assert_eq!(served, Served::REFUSED);
        }
        let unknown = ObjectHash([0xAB; 32]);
        let (served, _) = serve(
            &peer,
            &node,
            request(unknown, 0, 1),
            &unadvertised,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::REFUSED, "not even a miss is answered");

        // The bit is advertised, but there is no index.
        let (_index_tx, advertised) = serving(index, true);
        let without_index = ObjectServing {
            source: None,
            ..advertised.clone()
        };
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 1024),
            &without_index,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::REFUSED);

        // Both hold: served.
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 1024),
            &advertised,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            served,
            Served::Found {
                size: size_of(&data),
                data,
            }
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Misses and probes are answered from the index alone: an object the index
/// does not hold is not found, a zero length or an offset at or past the end
/// answers with the size and no data, and none of them opens the object's
/// file or takes a permit. The offset is checked against the size first, as
/// the draft orders, so a probe at the end with a nonzero length is still a
/// probe.
#[tokio::test]
async fn object_probes_and_misses_are_answered_from_the_index() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (peer, node, _client_ep, _server_ep) = connected_pair(&network).await;
        // The index names a file that does not exist: any disk read would fail, and the request
        // would be refused rather than answered.
        let hash = ObjectHash([0x11; 32]);
        let size = 1000;
        let entry = ObjectEntry {
            path: "/nonexistent/zebra-get-object-test".into(),
            size,
        };
        let (_index_tx, objects) = serving([(hash, entry)].into_iter().collect(), true);
        let deadline = || Instant::now() + AMPLE;

        let unknown = ObjectHash([0x22; 32]);
        let (served, _) = serve(
            &peer,
            &node,
            request(unknown, 0, 1),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::NotFound);

        let probes = [
            request(hash, 0, 0),
            request(hash, 999, 0),
            request(hash, size, 10),
            request(hash, size + 1, MAX_OBJECT_REQUEST_LEN),
            request(hash, MAX_OBJECT_OFFSET, MAX_OBJECT_REQUEST_LEN),
        ];
        for probe in probes {
            let (served, _) =
                serve(&peer, &node, probe, &objects, deadline(), Duration::ZERO).await;
            assert_eq!(served, Served::probe(size), "probe {probe:?}");
        }
        assert_permits_free(&objects);

        // The same entry needs the disk for its last byte: refused, because the file is missing.
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 999, 1),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::REFUSED);
        assert_permits_free(&objects);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Object data is served from the requested offset, in chunks, up to the
/// request's `length` or the end of the object, whichever comes first, and
/// the permits are returned after each response. A file shorter than the
/// index says finishes the response early with the bytes there are, rather
/// than resetting it.
#[tokio::test]
async fn object_data_is_served_within_the_request_budget() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (peer, node, _client_ep, _server_ep) = connected_pair(&network).await;
        let dir = tempfile::tempdir().expect("a temporary directory");
        // Three chunks, the last one partial.
        let len = 2 * OBJECT_READ_CHUNK + 777;
        let (hash, entry, data) = write_object(dir.path(), len);
        let size = size_of(&data);
        let (_index_tx, objects) = serving([(hash, entry.clone())].into_iter().collect(), true);
        let deadline = || Instant::now() + AMPLE;
        let whole = u32::try_from(len).expect("the test object is small");
        let chunk = u32::try_from(OBJECT_READ_CHUNK).expect("the chunk is small");

        let cases: [(u64, u32, &[u8]); 6] = [
            (0, whole, &data),
            (0, MAX_OBJECT_REQUEST_LEN, &data),
            // A range that starts inside the object and runs past its end.
            (1000, 2 * chunk, &data[1000..]),
            // A range that straddles a chunk boundary of the response.
            (500, chunk + 100, &data[500..500 + OBJECT_READ_CHUNK + 100]),
            (size - 10, 100, &data[len - 10..]),
            (
                // `OBJECT_READ_CHUNK` is 1 MiB, which fits in a `u64`.
                OBJECT_READ_CHUNK as u64 - 1,
                3,
                &data[OBJECT_READ_CHUNK - 1..OBJECT_READ_CHUNK + 2],
            ),
        ];
        for (offset, length, expected) in cases {
            let (served, _) = serve(
                &peer,
                &node,
                request(hash, offset, length),
                &objects,
                deadline(),
                Duration::ZERO,
            )
            .await;
            assert_eq!(
                served,
                Served::Found {
                    size,
                    data: expected.to_vec(),
                },
                "offset {offset} length {length}"
            );
            assert_permits_free(&objects);
        }

        // The index says the object is longer than its file.
        let claimed = size + 5000;
        let longer = ObjectEntry {
            size: claimed,
            ..entry
        };
        let (_index_tx, objects) = serving([(hash, longer)].into_iter().collect(), true);
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, MAX_OBJECT_REQUEST_LEN),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            served,
            Served::Found {
                size: claimed,
                data,
            }
        );
        assert_permits_free(&objects);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A request for data is refused while the peer's or the node's object stream
/// permits are all taken, and served again once one is free; probes are
/// answered regardless, because they take no permit.
#[tokio::test]
async fn object_streams_are_limited_per_peer_and_globally() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (peer, node, _client_ep, _server_ep) = connected_pair(&network).await;
        let dir = tempfile::tempdir().expect("a temporary directory");
        let (hash, entry, data) = write_object(dir.path(), 4096);
        let size = size_of(&data);
        let (_index_tx, objects) = serving([(hash, entry)].into_iter().collect(), true);
        let deadline = || Instant::now() + AMPLE;
        let found = Served::Found {
            size,
            data: data.clone(),
        };

        // Every one of the peer's permits is held by other streams of its own.
        let peer_streams = objects
            .peer_permits
            .clone()
            .acquire_many_owned(u32::try_from(MAX_OBJECT_STREAMS_PER_PEER).expect("a small limit"))
            .await
            .expect("the semaphore is open");
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 4096),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::REFUSED);
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 0),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::probe(size), "probes need no permit");
        drop(peer_streams);
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 4096),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, found);

        // Every one of the node's permits is held by other peers' streams.
        let other_peers = objects
            .global_permits
            .clone()
            .acquire_many_owned(u32::try_from(MAX_OBJECT_STREAMS_GLOBAL).expect("a small limit"))
            .await
            .expect("the semaphore is open");
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 4096),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::REFUSED);
        assert_eq!(
            objects.peer_permits.available_permits(),
            MAX_OBJECT_STREAMS_PER_PEER,
            "a refused request gives its peer permit back"
        );
        drop(other_peers);
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 4096),
            &objects,
            deadline(),
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, found);
        assert_permits_free(&objects);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A response that cannot finish before the margin ahead of its request
/// deadline is finished early with the bytes written so far, never reset:
/// a peer that reads slowly gets a short answer, and the node is done before
/// the deadline. A request that arrives with less than the margin left is
/// refused before anything is written.
#[tokio::test]
async fn object_responses_finish_early_before_the_deadline() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        // The peer grants the node a small receive window, so the response stalls on flow
        // control until the peer reads.
        let window = 64 * 1024;
        let client_config =
            crate::quic::client_config_with_receive_windows(&network, window, 2 * window);
        let (peer, node, _client_ep, _server_ep) =
            connected_pair_with(&network, Some(client_config)).await;
        let dir = tempfile::tempdir().expect("a temporary directory");
        let len = 4 * OBJECT_READ_CHUNK;
        let (hash, entry, data) = write_object(dir.path(), len);
        let size = size_of(&data);
        let (_index_tx, objects) = serving([(hash, entry)].into_iter().collect(), true);

        // The response may run for one second before the margin.
        let headroom = Duration::from_secs(1);
        let deadline = Instant::now() + OBJECT_EARLY_FIN_MARGIN + headroom;
        let (served, timing) = serve(
            &peer,
            &node,
            request(
                hash,
                0,
                u32::try_from(len).expect("the test object is small"),
            ),
            &objects,
            deadline,
            // The peer starts reading only after the node has given up writing.
            OBJECT_EARLY_FIN_MARGIN,
        )
        .await;
        let Served::Found {
            size: served_size,
            data: prefix,
        } = served
        else {
            panic!("a started response is finished, not reset: got {served:?}");
        };
        assert_eq!(served_size, size);
        assert!(
            !prefix.is_empty() && prefix.len() < len,
            "a short answer: {} of {len} bytes",
            prefix.len()
        );
        assert_eq!(prefix, data[..prefix.len()], "the prefix is the object's");
        // The node finished at the margin, not at once (timers fire a little early), and before
        // the peer read anything: a node that waited for the peer would finish after it.
        assert!(
            timing.node_finished + TIMER_SLACK >= headroom,
            "the node finished before the margin: {timing:?}"
        );
        assert!(
            timing.node_finished < timing.peer_read_from,
            "the node waited for the peer: {timing:?}"
        );
        assert_permits_free(&objects);

        // Less than the margin left: refused, nothing written.
        let deadline = Instant::now() + OBJECT_EARLY_FIN_MARGIN / 2;
        let (served, _) = serve(
            &peer,
            &node,
            request(hash, 0, 1),
            &objects,
            deadline,
            Duration::ZERO,
        )
        .await;
        assert_eq!(served, Served::REFUSED);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A response whose chunk write waits for the peer longer than
/// [`OBJECT_WRITE_STALL`] is finished early with the bytes written so far,
/// long before its deadline, so a peer that reads slowly cannot hold the
/// node's object stream permits until then.
#[tokio::test]
async fn object_responses_finish_early_when_a_write_stalls() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        // The peer grants the node a small receive window, so the response stalls on flow
        // control until the peer reads.
        let window = 64 * 1024;
        let client_config =
            crate::quic::client_config_with_receive_windows(&network, window, 2 * window);
        let (peer, node, _client_ep, _server_ep) =
            connected_pair_with(&network, Some(client_config)).await;
        let dir = tempfile::tempdir().expect("a temporary directory");
        let len = 4 * OBJECT_READ_CHUNK;
        let (hash, entry, data) = write_object(dir.path(), len);
        let (_index_tx, objects) = serving([(hash, entry)].into_iter().collect(), true);

        // The deadline is far beyond the stall limit.
        let deadline = Instant::now() + OBJECT_EARLY_FIN_MARGIN + 4 * OBJECT_WRITE_STALL;
        let (served, timing) = serve(
            &peer,
            &node,
            request(
                hash,
                0,
                u32::try_from(len).expect("the test object is small"),
            ),
            &objects,
            deadline,
            // The peer starts reading only after the node has given up writing.
            OBJECT_WRITE_STALL + Duration::from_secs(2),
        )
        .await;
        let Served::Found { data: prefix, .. } = served else {
            panic!("a started response is finished, not reset: got {served:?}");
        };
        assert!(
            !prefix.is_empty() && prefix.len() < len,
            "a short answer: {} of {len} bytes",
            prefix.len()
        );
        assert_eq!(prefix, data[..prefix.len()], "the prefix is the object's");
        // The node finished at the stall limit (timers fire a little early), well before the
        // deadline, and before the peer read anything.
        assert!(
            timing.node_finished + TIMER_SLACK >= OBJECT_WRITE_STALL,
            "the node finished before the stall limit: {timing:?}"
        );
        assert!(
            timing.node_finished < timing.peer_read_from,
            "the node waited for the peer: {timing:?}"
        );
        assert_permits_free(&objects);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A probe header write stops before the request deadline too. When another response uses up the
/// connection's flow-control credit, the write is cancelled at the margin because a header prefix
/// may already have been sent, not refused as though no response had started.
///
/// This calls [`serve_object`] directly on a real loopback stream, without an outer request
/// deadline: a header write that waited for the peer would be answered once the peer reads, which
/// the assertion rules out.
#[tokio::test]
async fn object_headers_are_written_before_the_margin() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        // The peer grants the node one small window for the whole connection, so one response it
        // does not read uses up the credit of every other stream.
        let window = 64 * 1024;
        let client_config =
            crate::quic::client_config_with_receive_windows(&network, window, window);
        let (peer, node, _client_ep, _server_ep) =
            connected_pair_with(&network, Some(client_config)).await;
        let dir = tempfile::tempdir().expect("a temporary directory");
        let len = 2 * OBJECT_READ_CHUNK;
        let (hash, entry, data) = write_object(dir.path(), len);
        let size = size_of(&data);
        let (_index_tx, objects) = serving([(hash, entry)].into_iter().collect(), true);

        // The data response fills the window at once, then stalls until its margin, two seconds
        // in; the peer reads it after six.
        let data_headroom = Duration::from_secs(2);
        let peer_reads_after = Duration::from_secs(6);
        let data_response = serve(
            &peer,
            &node,
            request(
                hash,
                0,
                u32::try_from(len).expect("the test object is small"),
            ),
            &objects,
            Instant::now() + OBJECT_EARLY_FIN_MARGIN + data_headroom,
            peer_reads_after,
        );

        // Half a second later, a probe on a second stream, whose header has no credit to go out
        // on; its margin is a second after it starts.
        let probe_starts_after = Duration::from_millis(500);
        let probe_headroom = Duration::from_secs(1);
        let probe = async {
            tokio::time::sleep(probe_starts_after).await;
            serve(
                &peer,
                &node,
                request(hash, 0, 0),
                &objects,
                Instant::now() + OBJECT_EARLY_FIN_MARGIN + probe_headroom,
                Duration::ZERO,
            )
            .await
        };
        let ((data_served, data_timing), (probe_served, probe_timing)) =
            tokio::join!(data_response, probe);

        // The header write was cancelled at its margin: not refused as though no response had
        // started, and not answered once the peer read the data response and freed the credit.
        assert_eq!(
            probe_served,
            Served::Reset(ErrorCode::Cancelled.code()),
            "{probe_timing:?}"
        );
        assert!(
            probe_timing.node_finished + TIMER_SLACK >= probe_headroom,
            "the probe was cancelled before its margin: {probe_timing:?}"
        );
        assert!(
            probe_timing.node_finished + probe_starts_after < peer_reads_after,
            "the probe waited for the peer: {probe_timing:?}"
        );

        // The data response was finished early with what the window took.
        let Served::Found {
            size: served_size,
            data: prefix,
        } = data_served
        else {
            panic!("a started response is finished, not reset: got {data_served:?}");
        };
        assert_eq!(served_size, size);
        assert!(
            !prefix.is_empty() && prefix.len() < len,
            "a short answer: {} of {len} bytes",
            prefix.len()
        );
        assert_eq!(prefix, data[..prefix.len()], "the prefix is the object's");
        assert!(
            data_timing.node_finished < data_timing.peer_read_from,
            "the node waited for the peer: {data_timing:?}"
        );
        assert_permits_free(&objects);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A stale index entry is peer-triggerable, so its file error is logged at
/// debug rather than info.
#[test]
#[tracing_test::traced_test]
fn stale_object_errors_are_logged_at_debug() {
    unreadable(
        &ObjectHash([0xAB; 32]),
        &std::io::Error::new(std::io::ErrorKind::NotFound, "missing test object"),
        "could not open a served object",
    );
    assert!(!logs_contain("INFO"));
    if cfg!(debug_assertions) {
        assert!(logs_contain("DEBUG"));
        assert!(logs_contain("could not open a served object"));
    }
}

/// A `get-object` stream takes none of the peer's request permits, so a
/// peer's excess object requests reach the object permits and are refused
/// there, rather than queued behind its other requests until the deadline;
/// every other request type waits for a permit.
#[tokio::test]
async fn object_streams_take_no_request_permit() {
    let _init_guard = zebra_test::init();

    let exhausted = Arc::new(Semaphore::new(0));
    let Some(Ok(permit)) = request_permit(StreamType::GetObject, exhausted.clone()).now_or_never()
    else {
        panic!("an object stream waits for a request permit, or the permits were closed");
    };
    assert!(permit.is_none(), "an object stream holds no request permit");
    assert!(
        request_permit(StreamType::GetHeaders, exhausted.clone())
            .now_or_never()
            .is_none(),
        "other requests wait for a permit"
    );

    let free = Arc::new(Semaphore::new(1));
    let Some(Ok(permit)) = request_permit(StreamType::GetHeaders, free.clone()).now_or_never()
    else {
        panic!("a free permit is taken at once");
    };
    assert!(permit.is_some(), "other requests hold their permit");
    assert_eq!(free.available_permits(), 0);
    drop(permit);
    assert_eq!(free.available_permits(), 1);
}
