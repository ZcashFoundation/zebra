//! Loopback tests for this node's `get-object` requests (draft,
//! "`get-object`"): a node connected to a raw peer that answers each request
//! with crafted bytes on its stream.

use std::time::{Duration, Instant};

use futures::StreamExt;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use tower::{service_fn, util::BoxCloneService, Service, ServiceExt};

use zebra_chain::{block, parameters::Network};

use crate::{
    constants::{CURRENT_NETWORK_PROTOCOL_VERSION, MAX_PEER_MISBEHAVIOR_SCORE, REQUEST_TIMEOUT},
    peer::{
        v2::{
            connection::{
                global_object_permits, outbound::ObjectSizes, spawn_connection, ConnectionParams,
            },
            handshake::Role,
            misbehavior::MisbehaviorScores,
            nonces::NonceRegistry,
            tests::{connected_pair, identity, run_handshake},
        },
        Client, SharedPeerError,
    },
    peer_set::ActiveConnectionCounter,
    protocol::{
        external::types::PeerServices,
        internal::{Request, Response},
        v2::{
            constants::{MAX_OBJECT_OFFSET, MAX_OBJECT_REQUEST_LEN, RESPONSE_MISBEHAVIOR_PENALTY},
            handshake::InitRecord,
            object::ObjectHash,
            record::RecordCodec,
            request::{GetObject, Request as WireRequest},
            response::{encode_object_header, ObjectAnswer},
            types::{ErrorCode, StreamType},
        },
    },
    quic::app_error_code,
    BoxError, MisbehaviorReport, PeerSocketAddr,
};

/// Every loopback exchange finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a test waits to see that something does not happen.
const SHORT_WAIT: Duration = Duration::from_millis(500);

/// How long a test waits for the node to close a connection it must close.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// A node whose peer is a raw QUIC connection that completed the handshake
/// by hand, so the test can answer the node's requests with any bytes.
struct RawServer {
    /// The raw peer's side of the connection.
    connection: Connection,

    /// The node's client for the raw peer.
    client: Client,

    /// The peers the node's misbehavior scores reported for banning.
    misbehavior_rx: tokio::sync::mpsc::Receiver<MisbehaviorReport>,

    /// The node's misbehavior scores, which the test probes for the raw
    /// peer's score.
    scores: MisbehaviorScores,

    /// The raw peer's address, as the node sees it.
    peer: PeerSocketAddr,

    /// The raw peer's handshake stream, kept open for the connection's life.
    _handshake: (SendStream, RecvStream),

    _endpoints: (Endpoint, Endpoint),
}

impl RawServer {
    /// Connects a raw peer that advertises `services` to a node; the raw
    /// peer dials. The node's inbound service answers everything with `Nil`.
    async fn connect(services: PeerServices) -> Self {
        let network = Network::Mainnet;
        let (connection, inbound, client_ep, server_ep) = connected_pair(&network).await;

        let init = InitRecord {
            version: CURRENT_NETWORK_PROTOCOL_VERSION,
            services,
            nonce: Default::default(),
            user_agent: "/raw/".to_string(),
            start_height: block::Height(0),
            relay: true,
            announce: false,
            full_ids: false,
        };
        let (mut hs_send, hs_recv) = connection.open_bi().await.expect("opens");
        hs_send
            .write_all(&[StreamType::Handshake.code()])
            .await
            .expect("writes");
        hs_send
            .write_all(&RecordCodec::encode_record(init.encode()).expect("valid"))
            .await
            .expect("writes");
        let (node_identity, nonces) = (identity("/node/"), NonceRegistry::spawn(8));
        let (completed, hs_recv) = tokio::join!(
            run_handshake(&inbound, Role::Responder, &network, &node_identity, &nonces),
            async {
                let mut framed = tokio_util::codec::FramedRead::new(hs_recv, RecordCodec);
                framed.next().await.expect("init").expect("valid");
                framed.into_inner()
            }
        );

        let (misbehavior_tx, misbehavior_rx) = tokio::sync::mpsc::channel(8);
        let scores = MisbehaviorScores::spawn(misbehavior_tx, 8);
        let peer = PeerSocketAddr::from(inbound.remote_address());
        let inbound_service = BoxCloneService::new(service_fn(|_: Request| async {
            Ok::<Response, BoxError>(Response::Nil)
        }));
        let (client, _closer) = spawn_connection(
            inbound,
            completed.expect("the handshake completes"),
            ActiveConnectionCounter::new_counter().track_connection(),
            ConnectionParams {
                inbound_service,
                inv_collector: tokio::sync::broadcast::channel(16).0,
                genesis_hash: network.genesis_hash(),
                misbehavior: scores.clone(),
                address_book: None,
                discovery: false,
                bans: tokio::sync::watch::channel(crate::BanList::default()).1,
                addr_relay: None,
                serve_block_ranges: true,
                block_range_permits: crate::peer::v2::connection::global_block_range_permits(),
                hashes_permits: crate::peer::v2::connection::global_hashes_permits(),
                object_source: None,
                local_services: PeerServices::NODE_NETWORK,
                object_permits: global_object_permits(),
            },
            None,
        );

        RawServer {
            connection,
            client,
            misbehavior_rx,
            scores,
            peer,
            _handshake: (hs_send, hs_recv),
            _endpoints: (client_ep, server_ep),
        }
    }

    /// Asserts that the node closes the connection with `code`, soon.
    async fn assert_closed_with(&self, code: ErrorCode, case: &str) {
        let close = tokio::time::timeout(CLOSE_TIMEOUT, self.connection.closed())
            .await
            .unwrap_or_else(|_| panic!("{case}: expected a close with {code}"));
        assert!(
            matches!(&close, quinn::ConnectionError::ApplicationClosed(close)
                if close.error_code.into_inner() == code.code()),
            "{case}: expected {code}, got {close:?}"
        );
    }

    /// Asserts that the connection is open and usable.
    async fn assert_open(&mut self, case: &str) {
        call(&mut self.client, Request::Ping(Default::default()))
            .await
            .unwrap_or_else(|error| panic!("{case}: the client fails: {error}"));
        assert!(
            self.connection.close_reason().is_none(),
            "{case}: the connection stays open"
        );
    }

    /// Asserts that no answer on the connection added to the peer's
    /// misbehavior score, and that the peer was not reported for banning.
    ///
    /// One [`RESPONSE_MISBEHAVIOR_PENALTY`] stays below the ban threshold, so
    /// the test probes the score with the points that reach the threshold on
    /// top of one penalty, but not on their own. The probe changes the score,
    /// so a test checks this once for each connection, at its end.
    async fn assert_unscored(mut self, case: &str) {
        assert!(
            self.misbehavior_rx.try_recv().is_err(),
            "{case}: the peer is not reported for banning"
        );
        let probe = MAX_PEER_MISBEHAVIOR_SCORE - RESPONSE_MISBEHAVIOR_PENALTY + 1;
        assert!(
            !self.scores.penalize(self.peer, probe, "test probe").await,
            "{case}: an answer added to the peer's misbehavior score"
        );
    }

    /// Asserts that the connection is open and usable, and that no answer
    /// added to the peer's misbehavior score.
    async fn assert_open_and_unscored(mut self, case: &str) {
        self.assert_open(case).await;
        self.assert_unscored(case).await;
    }
}

/// Sends `request` through `client`.
async fn call(client: &mut Client, request: Request) -> Result<Response, SharedPeerError> {
    client.ready().await?;
    client.call(request).await
}

/// A request for up to `length` bytes from `offset` of the object whose hash
/// is 32 `hash` bytes.
fn object_request(hash: u8, offset: u64, length: u32) -> Request {
    Request::Object {
        hash: ObjectHash([hash; 32]),
        offset,
        length,
    }
}

/// A found answer for an object of `size` bytes, carrying `data`.
fn found(size: u64, data: &[u8]) -> Vec<u8> {
    let mut answer = encode_object_header(ObjectAnswer::Found { size }).expect("valid");
    answer.extend_from_slice(data);
    answer
}

/// The not-found answer.
fn not_found() -> Vec<u8> {
    encode_object_header(ObjectAnswer::NotFound).expect("valid")
}

/// How the raw peer answers one of the node's requests.
enum Reply {
    /// Writes these bytes, then finishes the stream.
    Finish(Vec<u8>),

    /// Resets the stream with this code, without an answer.
    Reset(ErrorCode),
}

/// Accepts the node's next request stream, checks that it carries one
/// complete `get-object` request, and answers it with `reply`. Returns the
/// request.
async fn answer(raw: &Connection, reply: Reply) -> GetObject {
    let (mut send, mut recv) = raw
        .accept_bi()
        .await
        .expect("the node opens a request stream");
    let request = recv.read_to_end(1 << 10).await.expect("a complete request");
    assert_eq!(request[0], StreamType::GetObject.code());
    let WireRequest::Object(request) =
        WireRequest::decode(StreamType::GetObject, &request[1..]).expect("a valid request")
    else {
        panic!("the node sent a get-object request on a get-object stream")
    };

    // The node may close the connection as soon as it reads a bad answer.
    match reply {
        Reply::Finish(bytes) => {
            let _ = send.write_all(&bytes).await;
            let _ = send.finish();
        }
        Reply::Reset(code) => {
            let _ = send.reset(app_error_code(code));
        }
    }
    request
}

/// A `get-object` request goes out as the draft's layout, and the answer
/// comes back as the object's size and the bytes delivered: the whole
/// object, a range, a range the responder finished early, nothing at or past
/// the end, or not found. None of these costs the peer anything.
#[tokio::test]
async fn object_answers_carry_the_delivered_range() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;
        let object: Vec<u8> = (0..=255).cycle().take(1000).collect();

        // Each case asks for its own object, so the sizes never conflict.
        let cases = [
            (
                "whole object",
                (0, 2000),
                found(1000, &object),
                Response::Object {
                    size: 1000,
                    data: object.clone(),
                },
            ),
            (
                "range",
                (400, 100),
                found(1000, &object[400..500]),
                Response::Object {
                    size: 1000,
                    data: object[400..500].to_vec(),
                },
            ),
            (
                "finished early",
                (0, 1000),
                found(1000, &object[..10]),
                Response::Object {
                    size: 1000,
                    data: object[..10].to_vec(),
                },
            ),
            (
                "at the end",
                (1000, 5),
                found(1000, &[]),
                Response::Object {
                    size: 1000,
                    data: Vec::new(),
                },
            ),
            (
                "past the end",
                (MAX_OBJECT_OFFSET, MAX_OBJECT_REQUEST_LEN),
                found(1000, &[]),
                Response::Object {
                    size: 1000,
                    data: Vec::new(),
                },
            ),
            (
                "empty object",
                (0, 5),
                found(0, &[]),
                Response::Object {
                    size: 0,
                    data: Vec::new(),
                },
            ),
            ("not found", (0, 5), not_found(), Response::ObjectNotFound),
        ];

        for (hash, (case, (offset, length), reply, expected)) in (1..).zip(cases) {
            let (response, request) = tokio::join!(
                call(&mut raw.client, object_request(hash, offset, length)),
                answer(&raw.connection, Reply::Finish(reply)),
            );
            assert_eq!(
                request,
                GetObject {
                    hash: ObjectHash([hash; 32]),
                    offset,
                    length,
                },
                "{case}: the request on the wire"
            );
            assert_eq!(response.expect(case), expected, "{case}");
        }

        raw.assert_open_and_unscored("valid answers").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An answer that breaks the draft is a connection error with its code and
/// no misbehavior score (draft, "Misbehavior and Banning"): more data than
/// the request's length, or data past the end of the object as the answer's
/// own size gives it, is `FLOOD`; an answer that does not conform to the
/// format is `PROTOCOL_ERROR`.
#[tokio::test]
async fn object_answer_violations_close_the_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        for (case, (offset, length), reply, code) in [
            (
                "more than the length",
                (0, 4),
                found(100, &[1; 5]),
                ErrorCode::Flood,
            ),
            (
                "past the end of the object",
                (8, 100),
                found(10, &[1; 3]),
                ErrorCode::Flood,
            ),
            (
                "data at the end of the object",
                (10, 5),
                found(10, &[1]),
                ErrorCode::Flood,
            ),
            ("no answer", (0, 5), Vec::new(), ErrorCode::ProtocolError),
            (
                "unknown result",
                (0, 5),
                vec![0x01],
                ErrorCode::ProtocolError,
            ),
            ("no size", (0, 5), vec![0x00], ErrorCode::ProtocolError),
            (
                "truncated size",
                (0, 5),
                vec![0x00, 0xFD, 0x05],
                ErrorCode::ProtocolError,
            ),
            (
                "non-canonical size",
                (0, 5),
                vec![0x00, 0xFD, 0x05, 0x00],
                ErrorCode::ProtocolError,
            ),
            (
                "data after not found",
                (0, 5),
                [not_found(), vec![0]].concat(),
                ErrorCode::ProtocolError,
            ),
        ] {
            let mut raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;

            let (response, _request) = tokio::join!(
                call(&mut raw.client, object_request(1, offset, length)),
                answer(&raw.connection, Reply::Finish(reply)),
            );
            let error = response.expect_err(case);
            assert!(
                error.inner_debug().contains("ProtocolViolation"),
                "{case}: got {}",
                error.inner_debug()
            );
            raw.assert_closed_with(code, case).await;
            raw.assert_unscored(case).await;
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A request the peer resets before its answer fails, and the connection
/// stays open: a refusal is not misbehavior (draft, "Request Streams"). A
/// `REFUSED` reset fails with its own error, `RequestRefused`, so a caller can
/// back off from a busy peer; another code is a stream failure.
#[tokio::test]
async fn reset_object_requests_fail_and_keep_the_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;

        for (code, expected, whole) in [
            (ErrorCode::Refused, "RequestRefused", true),
            (ErrorCode::Cancelled, "RequestStreamFailed(", false),
        ] {
            let (response, _request) = tokio::join!(
                call(&mut raw.client, object_request(1, 0, 5)),
                answer(&raw.connection, Reply::Reset(code)),
            );
            let inner = response.expect_err("reset").inner_debug();
            if whole {
                // `Request::Object` tells a caller to compare the whole string.
                assert_eq!(inner, expected, "{code}");
            } else {
                assert!(inner.starts_with(expected), "{code}: got {inner}");
            }
            raw.assert_open(code.name()).await;
        }
        raw.assert_unscored("reset requests").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// This node checks its own requests against the draft's bounds: one with an
/// offset above 2^48 − 1 or a length above 32 MiB fails with
/// `RequestOutOfBounds` and is never sent, and the connection stays open;
/// one at the bounds is sent.
#[tokio::test]
async fn out_of_bounds_object_requests_are_not_sent() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;

        for (offset, length) in [
            (MAX_OBJECT_OFFSET + 1, 1),
            (u64::MAX, 1),
            (0, MAX_OBJECT_REQUEST_LEN + 1),
            (0, u32::MAX),
        ] {
            let error = call(&mut raw.client, object_request(1, offset, length))
                .await
                .expect_err("out of bounds");
            assert!(
                error.inner_debug().contains("RequestOutOfBounds"),
                "offset {offset}, length {length}: got {}",
                error.inner_debug()
            );
        }
        assert!(
            tokio::time::timeout(SHORT_WAIT, raw.connection.accept_bi())
                .await
                .is_err(),
            "no request stream was opened"
        );
        raw.assert_open("out of bounds").await;

        let (response, request) = tokio::join!(
            call(
                &mut raw.client,
                object_request(2, MAX_OBJECT_OFFSET, MAX_OBJECT_REQUEST_LEN)
            ),
            answer(&raw.connection, Reply::Finish(found(10, &[]))),
        );
        assert_eq!(
            (request.offset, request.length),
            (MAX_OBJECT_OFFSET, MAX_OBJECT_REQUEST_LEN)
        );
        assert_eq!(
            response.expect("at the bounds"),
            Response::Object {
                size: 10,
                data: Vec::new()
            }
        );
        raw.assert_open_and_unscored("bounds").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An answer whose size differs from the one an earlier answer on the same
/// connection gave for the object closes the connection with
/// `PROTOCOL_ERROR`, and no misbehavior score (draft, "`get-object`").
/// Answers that agree, another object's size, and a not-found answer for an
/// object the peer gave a size for do not; and another connection may give
/// another size.
#[tokio::test]
async fn changed_object_sizes_close_the_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;

        for (case, hash, offset, reply, expected) in [
            (
                "first range",
                1,
                0,
                found(100, &[1; 5]),
                Response::Object {
                    size: 100,
                    data: vec![1; 5],
                },
            ),
            (
                "same size",
                1,
                5,
                found(100, &[2; 5]),
                Response::Object {
                    size: 100,
                    data: vec![2; 5],
                },
            ),
            (
                "another object",
                2,
                0,
                found(7, &[3; 5]),
                Response::Object {
                    size: 7,
                    data: vec![3; 5],
                },
            ),
            ("not found", 1, 0, not_found(), Response::ObjectNotFound),
        ] {
            let (response, _request) = tokio::join!(
                call(&mut raw.client, object_request(hash, offset, 5)),
                answer(&raw.connection, Reply::Finish(reply)),
            );
            assert_eq!(response.expect(case), expected, "{case}");
        }
        raw.assert_open("consistent sizes").await;

        let (response, _request) = tokio::join!(
            call(&mut raw.client, object_request(1, 10, 5)),
            answer(&raw.connection, Reply::Finish(found(101, &[4; 5]))),
        );
        let error = response.expect_err("a changed size");
        assert!(
            error.inner_debug().contains("ProtocolViolation"),
            "got {}",
            error.inner_debug()
        );
        raw.assert_closed_with(ErrorCode::ProtocolError, "changed size")
            .await;
        raw.assert_unscored("changed size").await;

        // The sizes belong to the connection.
        let mut other = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;
        let (response, _request) = tokio::join!(
            call(&mut other.client, object_request(1, 0, 5)),
            answer(&other.connection, Reply::Finish(found(101, &[5; 5]))),
        );
        assert_eq!(
            response.expect("a new connection"),
            Response::Object {
                size: 101,
                data: vec![5; 5],
            }
        );
        other.assert_open_and_unscored("another connection").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A connection remembers the sizes of the last 1,024 objects it asked about:
/// the first size given for an object is kept, and a different one is reported;
/// beyond the bound, the object first asked about is forgotten first.
#[tokio::test]
async fn object_sizes_are_bounded_and_forget_the_oldest() {
    let _init_guard = zebra_test::init();

    let sizes = ObjectSizes::spawn();
    let hash = |index: u16| {
        let mut hash = [0; 32];
        hash[..2].copy_from_slice(&index.to_le_bytes());
        ObjectHash(hash)
    };

    assert_eq!(sizes.record(hash(0), 10).await, None, "the first size");
    assert_eq!(sizes.record(hash(0), 10).await, None, "the same size");
    assert_eq!(sizes.record(hash(0), 11).await, Some(10), "a changed size");
    assert_eq!(
        sizes.record(hash(0), 10).await,
        None,
        "the first size is kept"
    );

    for index in 1..1_024 {
        assert_eq!(sizes.record(hash(index), 10).await, None);
    }
    assert_eq!(
        sizes.record(hash(0), 11).await,
        Some(10),
        "1,024 objects are remembered"
    );

    // The next object replaces the oldest, and each one after it replaces the next oldest.
    assert_eq!(sizes.record(hash(1_024), 10).await, None);
    assert_eq!(
        sizes.record(hash(0), 11).await,
        None,
        "the oldest object is forgotten"
    );
    assert_eq!(
        sizes.record(hash(1), 11).await,
        None,
        "the next oldest is forgotten for it"
    );
    assert_eq!(
        sizes.record(hash(3), 11).await,
        Some(10),
        "newer objects are remembered"
    );
    assert_eq!(
        sizes.record(hash(1_024), 11).await,
        Some(10),
        "newer objects are remembered"
    );
}

/// Accepts the node's next request stream and reads its `get-object`
/// request, returning the request and the stream the test answers on.
async fn accept_object_stream(raw: &Connection) -> (GetObject, SendStream) {
    let (send, mut recv) = raw
        .accept_bi()
        .await
        .expect("the node opens a request stream");
    let request = recv.read_to_end(1 << 10).await.expect("a complete request");
    assert_eq!(request[0], StreamType::GetObject.code());
    match WireRequest::decode(StreamType::GetObject, &request[1..]).expect("a valid request") {
        WireRequest::Object(request) => (request, send),
        request => panic!("expected a get-object request, got {request:?}"),
    }
}

/// An answer the responder resets after its size, with `CANCELLED` or any
/// other code, is a short answer with the data the node read before the
/// reset, not a failure, and the connection stays open.
///
/// The raw peer sends far more data than the receive window the node grants
/// a stream before it resets: flow control only lets the last bytes through
/// once the node has read the first ones, so most of the data is read by
/// then, however the two sides are scheduled.
#[tokio::test]
async fn reset_object_answers_keep_the_data_received() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;
        let size = MAX_OBJECT_REQUEST_LEN;
        let sent: Vec<u8> = (0..=250).cycle().take(24 << 20).collect();

        for (hash, code) in [(1, ErrorCode::Cancelled), (2, ErrorCode::Refused)] {
            let (response, ()) = tokio::join!(
                call(&mut raw.client, object_request(hash, 0, size)),
                async {
                    let (_request, mut send) = accept_object_stream(&raw.connection).await;
                    send.write_all(&found(size.into(), &sent))
                        .await
                        .expect("the node reads the data");
                    let _ = send.reset(app_error_code(code));
                },
            );

            let Response::Object { size: got, data } = response.expect("a short answer") else {
                panic!("{code}: expected an object answer");
            };
            assert_eq!(got, u64::from(size), "{code}");
            assert!(
                data.len() >= 1 << 20,
                "{code}: only {} bytes were kept",
                data.len()
            );
            assert!(sent.starts_with(&data), "{code}: a prefix of the data");
            raw.assert_open(code.name()).await;
        }
        raw.assert_unscored("reset answers").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An answer still arriving at the request deadline is a short answer with
/// the data read so far, and the connection stays open; a
/// request whose answer never started still fails at the deadline.
///
/// This takes `REQUEST_TIMEOUT` of real time: pausing tokio's clock is
/// unsafe with real sockets.
#[tokio::test]
async fn object_answers_at_the_deadline_keep_the_data_received() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;
        let started = Instant::now();

        // An answer that stops after 100 of the 1,000 bytes asked for, without finishing.
        raw.client.ready().await.expect("ready");
        let slow = raw.client.call(object_request(1, 0, 1_000));
        let (_request, mut slow_send) = accept_object_stream(&raw.connection).await;
        slow_send
            .write_all(&found(1_000, &[7; 100]))
            .await
            .expect("writes");

        // An answer that never starts.
        raw.client.ready().await.expect("ready");
        let silent = raw.client.call(object_request(2, 0, 1_000));
        let (_request, silent_send) = accept_object_stream(&raw.connection).await;

        let (slow, silent) = tokio::join!(slow, silent);
        assert!(started.elapsed() >= REQUEST_TIMEOUT - Duration::from_secs(1));
        assert_eq!(
            slow.expect("a short answer"),
            Response::Object {
                size: 1_000,
                data: vec![7; 100],
            }
        );
        let error = silent.expect_err("no answer");
        assert!(
            error.inner_debug().contains("ConnectionReceiveTimeout"),
            "got {}",
            error.inner_debug()
        );

        // The raw peer held both streams open until now.
        drop((slow_send, silent_send));
        raw.assert_open_and_unscored("deadline").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `get-object` request body: the hash, the offset, and the length.
fn get_object_body(offset: u64, length: u32) -> Vec<u8> {
    let mut body = vec![0xAB; 32];
    body.extend_from_slice(&offset.to_le_bytes());
    body.extend_from_slice(&length.to_le_bytes());
    body
}

/// A peer's `get-object` request is read in full and decoded: an offset above
/// 2^48 − 1, a length above 32 MiB, or a body that is not exactly 44 bytes
/// closes the connection with `PROTOCOL_ERROR` (draft, "`get-object`",
/// "Request Streams"), and a valid request does not.
#[tokio::test]
async fn peer_object_requests_are_checked_against_the_draft() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        for (case, body) in [
            ("offset", get_object_body(MAX_OBJECT_OFFSET + 1, 1)),
            ("length", get_object_body(0, MAX_OBJECT_REQUEST_LEN + 1)),
            ("short body", get_object_body(0, 1)[..43].to_vec()),
            ("long body", [get_object_body(0, 1), vec![0]].concat()),
            (
                "valid",
                get_object_body(MAX_OBJECT_OFFSET, MAX_OBJECT_REQUEST_LEN),
            ),
        ] {
            let raw = RawServer::connect(PeerServices::NODE_SYNC_ARTIFACTS).await;

            let (mut send, mut recv) = raw.connection.open_bi().await.expect("opens");
            send.write_all(&[StreamType::GetObject.code()])
                .await
                .expect("writes");
            send.write_all(&body).await.expect("writes");
            send.finish().expect("finishes");

            if case == "valid" {
                // Answered or refused, by whether this node serves the object.
                let _ = recv.read_to_end(1 << 20).await;
                tokio::time::sleep(SHORT_WAIT).await;
                assert!(
                    raw.connection.close_reason().is_none(),
                    "{case}: the connection stays open"
                );
            } else {
                raw.assert_closed_with(ErrorCode::ProtocolError, case).await;
            }
        }
    })
    .await
    .expect("the test completes well within the timeout");
}
