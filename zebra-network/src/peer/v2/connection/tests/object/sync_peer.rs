//! Loopback tests for [`SyncPeer::get_object`]: a sync peer handle on one
//! side of a QUIC connection, and a raw peer on the other that answers each
//! request with crafted bytes.

use crate::sync_peer::{ConnectionId, ObjectChunk, SyncPeer, SyncPeerError, BULK_STREAMS_PER_PEER};

use super::*;

/// A sync peer handle over a real version 2 connection task on a fresh
/// loopback connection, and the raw peer's side of that connection.
struct RawSyncPeer {
    /// The handle under test.
    peer: SyncPeer,

    /// The raw peer's side of the connection.
    raw: Connection,

    /// The connection task's client, kept so the task keeps running.
    _client: Client,
    /// The raw peer's handshake streams, kept open for the connection's life.
    _raw_handshake: crate::peer::v2::handshake::CompletedHandshake,
    _misbehavior_rx: tokio::sync::mpsc::Receiver<MisbehaviorReport>,
    _endpoints: (Endpoint, Endpoint),
}

impl RawSyncPeer {
    /// Connects a sync peer handle to a raw peer, which completes the
    /// handshake and then answers the handle's requests by hand. The raw peer
    /// dials, so the node does not send it a `get-addr` request.
    async fn connect() -> Self {
        let network = Network::Mainnet;
        let (raw, node, raw_ep, node_ep) = connected_pair(&network).await;
        let (raw_identity, node_identity) = (identity("/raw/"), identity("/node/"));
        let (raw_nonces, node_nonces) = (NonceRegistry::spawn(8), NonceRegistry::spawn(8));
        let (raw_handshake, node_handshake) = tokio::join!(
            run_handshake(&raw, Role::Initiator, &network, &raw_identity, &raw_nonces),
            run_handshake(
                &node,
                Role::Responder,
                &network,
                &node_identity,
                &node_nonces
            ),
        );
        let raw_handshake = raw_handshake.expect("the handshake completes");
        let node_handshake = node_handshake.expect("the handshake completes");

        let (misbehavior_tx, misbehavior_rx) = tokio::sync::mpsc::channel(1);
        let misbehavior = MisbehaviorScores::spawn(misbehavior_tx, 1);
        let object_sizes = ObjectSizes::spawn();
        let (client, closer) = spawn_connection(
            node.clone(),
            node_handshake,
            ActiveConnectionCounter::new_counter().track_connection(),
            ConnectionParams {
                inbound_service: service_fn(|_| async { Ok::<_, BoxError>(Response::Nil) }),
                inv_collector: tokio::sync::broadcast::channel(16).0,
                genesis_hash: network.genesis_hash(),
                network: network.clone(),
                misbehavior: misbehavior.clone(),
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
            object_sizes.clone(),
            None,
        );
        let peer = SyncPeer::new(
            ConnectionId(1),
            PeerSocketAddr::from(node.remote_address()),
            PeerServices::NODE_SYNC_ARTIFACTS,
            block::Height(0),
            node,
            misbehavior,
            closer,
            object_sizes,
        );
        RawSyncPeer {
            peer,
            raw,
            _client: client,
            _raw_handshake: raw_handshake,
            _misbehavior_rx: misbehavior_rx,
            _endpoints: (raw_ep, node_ep),
        }
    }

    /// Asserts that the handle closed the connection with `code`, soon.
    async fn assert_closed_with(&self, code: ErrorCode, case: &str) {
        let close = tokio::time::timeout(CLOSE_TIMEOUT, self.raw.closed())
            .await
            .unwrap_or_else(|_| panic!("{case}: expected a close with {code}"));
        assert!(
            matches!(&close, quinn::ConnectionError::ApplicationClosed(close)
                if close.error_code.into_inner() == code.code()),
            "{case}: expected {code}, got {close:?}"
        );
    }

    /// Asserts that the connection is still open.
    fn assert_open(&self, case: &str) {
        assert!(
            self.raw.close_reason().is_none(),
            "{case}: the connection stays open"
        );
    }
}

/// Found answers carry the object's claimed size and the bytes delivered,
/// including a short answer the responder finished early, and not found is an
/// answer, not an error. The request goes out as asked, and the connection
/// stays open.
#[tokio::test]
async fn sync_peer_object_answers_carry_the_delivered_range() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = RawSyncPeer::connect().await;
        let object: Vec<u8> = (0..=255).cycle().take(1000).collect();
        let hash = ObjectHash([7; 32]);

        for (case, (offset, length), reply, expected) in [
            (
                "whole object",
                (0, 2000),
                found(1000, &object),
                ObjectChunk::Found {
                    size: 1000,
                    data: object.clone().into(),
                },
            ),
            (
                "range",
                (400, 100),
                found(1000, &object[400..500]),
                ObjectChunk::Found {
                    size: 1000,
                    data: object[400..500].to_vec().into(),
                },
            ),
            (
                "finished early",
                (0, 1000),
                found(1000, &object[..10]),
                ObjectChunk::Found {
                    size: 1000,
                    data: object[..10].to_vec().into(),
                },
            ),
            ("not found", (0, 1000), not_found(), ObjectChunk::NotFound),
        ] {
            let (chunk, request) = tokio::join!(
                raw.peer.get_object(hash, offset, length),
                answer(&raw.raw, Reply::Finish(reply)),
            );
            assert_eq!(
                request,
                GetObject {
                    hash,
                    offset,
                    length
                },
                "{case}: the request goes out as asked"
            );
            assert_eq!(chunk.expect(case), expected, "{case}");
            raw.assert_open(case);
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A request outside the draft's bounds is not sent, and the connection
/// stays open.
#[tokio::test]
async fn sync_peer_out_of_bounds_object_requests_are_not_sent() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = RawSyncPeer::connect().await;

        for (case, offset, length) in [
            ("offset", MAX_OBJECT_OFFSET + 1, 1),
            ("length", 0, MAX_OBJECT_REQUEST_LEN + 1),
        ] {
            let error = raw
                .peer
                .get_object(ObjectHash([1; 32]), offset, length)
                .await
                .expect_err(case);
            assert!(
                matches!(error, SyncPeerError::LocalRequest(_)),
                "{case}: got {error:?}"
            );
        }
        assert!(
            tokio::time::timeout(SHORT_WAIT, raw.raw.accept_bi())
                .await
                .is_err(),
            "no request stream is opened"
        );
        raw.assert_open("out of bounds");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `REFUSED` reset before the answer is a refusal, which keeps the
/// connection open.
#[tokio::test]
async fn sync_peer_refused_object_requests_keep_the_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = RawSyncPeer::connect().await;

        let (result, _request) = tokio::join!(
            raw.peer.get_object(ObjectHash([1; 32]), 0, 5),
            answer(&raw.raw, Reply::Reset(ErrorCode::Refused)),
        );
        assert_eq!(result, Err(SyncPeerError::Refused));
        raw.assert_open("refused");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An answer that breaks the draft's bounds or format closes the connection
/// with the violation's code, which the error carries.
#[tokio::test]
async fn sync_peer_object_answer_violations_close_the_connection() {
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
                "unknown result",
                (0, 5),
                vec![0x01],
                ErrorCode::ProtocolError,
            ),
            (
                "data after not found",
                (0, 5),
                [not_found(), vec![0]].concat(),
                ErrorCode::ProtocolError,
            ),
        ] {
            let raw = RawSyncPeer::connect().await;

            let (result, _request) = tokio::join!(
                raw.peer.get_object(ObjectHash([1; 32]), offset, length),
                answer(&raw.raw, Reply::Finish(reply)),
            );
            let error = result.expect_err(case);
            assert!(
                matches!(&error, SyncPeerError::Violation { code: got, .. } if *got == code),
                "{case}: expected a {code} violation, got {error:?}"
            );
            raw.assert_closed_with(code, case).await;
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An answer whose size differs from an earlier answer for the same hash is
/// a `PROTOCOL_ERROR` violation, which closes the connection.
#[tokio::test]
async fn sync_peer_object_size_changes_close_the_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = RawSyncPeer::connect().await;
        let hash = ObjectHash([3; 32]);

        let (first, _request) = tokio::join!(
            raw.peer.get_object(hash, 0, 5),
            answer(&raw.raw, Reply::Finish(found(100, &[1; 5]))),
        );
        assert_eq!(
            first,
            Ok(ObjectChunk::Found {
                size: 100,
                data: vec![1; 5].into(),
            })
        );

        let (second, _request) = tokio::join!(
            raw.peer.get_object(hash, 5, 5),
            answer(&raw.raw, Reply::Finish(found(200, &[1; 5]))),
        );
        let error = second.expect_err("a changed size is a violation");
        assert!(
            matches!(
                &error,
                SyncPeerError::Violation {
                    code: ErrorCode::ProtocolError,
                    ..
                }
            ),
            "expected a PROTOCOL_ERROR violation, got {error:?}"
        );
        raw.assert_closed_with(ErrorCode::ProtocolError, "size change")
            .await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An answer reset part way, with any code, is a short `Found` with the data
/// received, and the connection stays open.
#[tokio::test]
async fn sync_peer_reset_object_answers_keep_the_data_received() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = RawSyncPeer::connect().await;
        let size = MAX_OBJECT_REQUEST_LEN;
        let sent: Vec<u8> = (0..=250).cycle().take(24 << 20).collect();

        for (hash, code) in [(1, ErrorCode::Cancelled), (2, ErrorCode::Refused)] {
            let (chunk, ()) = tokio::join!(
                raw.peer.get_object(ObjectHash([hash; 32]), 0, size),
                async {
                    let (_request, mut send) = accept_object_stream(&raw.raw).await;
                    send.write_all(&found(size.into(), &sent))
                        .await
                        .expect("the handle reads the data");
                    let _ = send.reset(app_error_code(code));
                },
            );

            let Ok(ObjectChunk::Found { size: got, data }) = chunk else {
                panic!("{code}: expected a short found answer, got {chunk:?}");
            };
            assert_eq!(got, u64::from(size), "{code}");
            assert!(
                data.len() >= 1 << 20,
                "{code}: only {} bytes were kept",
                data.len()
            );
            assert!(sent.starts_with(&data), "{code}: a prefix of the data");
            raw.assert_open(code.name());
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An answer still arriving at the request deadline is a short `Found` with
/// the data read so far, and the connection stays open; a request whose
/// answer never started fails with `Transport` at the deadline.
///
/// This takes `REQUEST_TIMEOUT` of real time: pausing tokio's clock is
/// unsafe with real sockets.
#[tokio::test]
async fn sync_peer_object_answers_at_the_deadline_keep_the_data_received() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = RawSyncPeer::connect().await;
        let started = Instant::now();

        // An answer that stops after 100 of the 1,000 bytes asked for, without finishing.
        let slow = raw.peer.get_object(ObjectHash([1; 32]), 0, 1_000);
        // An answer that never starts.
        let silent = raw.peer.get_object(ObjectHash([2; 32]), 0, 1_000);

        let (slow, silent, (slow_send, silent_send)) = tokio::join!(slow, silent, async {
            let (_request, mut slow_send) = accept_object_stream(&raw.raw).await;
            slow_send
                .write_all(&found(1_000, &[7; 100]))
                .await
                .expect("writes");
            let (_request, silent_send) = accept_object_stream(&raw.raw).await;
            // The raw peer holds both streams open until the requests end.
            (slow_send, silent_send)
        });
        assert!(started.elapsed() >= REQUEST_TIMEOUT - Duration::from_secs(1));
        assert_eq!(
            slow,
            Ok(ObjectChunk::Found {
                size: 1_000,
                data: vec![7; 100].into(),
            })
        );
        assert!(
            matches!(silent, Err(SyncPeerError::Transport(_))),
            "got {silent:?}"
        );

        drop((slow_send, silent_send));
        raw.assert_open("deadline");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Each request holds one of the handle's bulk permits until its answer
/// ends, so one more than [`BULK_STREAMS_PER_PEER`] concurrent requests
/// opens a stream only once an earlier one has been answered.
#[tokio::test]
async fn sync_peer_object_requests_take_bulk_permits() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = RawSyncPeer::connect().await;

        let requests: Vec<_> = (0..=BULK_STREAMS_PER_PEER)
            .map(|index| {
                let peer = raw.peer.clone();
                let hash = ObjectHash([u8::try_from(index).expect("a few requests"); 32]);
                tokio::spawn(async move { peer.get_object(hash, 0, 5).await })
            })
            .collect();

        let mut open = Vec::new();
        for _ in 0..BULK_STREAMS_PER_PEER {
            open.push(accept_object_stream(&raw.raw).await);
        }
        assert!(
            tokio::time::timeout(SHORT_WAIT, raw.raw.accept_bi())
                .await
                .is_err(),
            "a request beyond the permits waits for one"
        );

        // Answering one request releases its permit to the waiting request.
        let (_request, mut send) = open.pop().expect("a stream is open");
        send.write_all(&not_found()).await.expect("writes");
        send.finish().expect("finishes");
        let (_request, mut last) = accept_object_stream(&raw.raw).await;
        last.write_all(&not_found()).await.expect("writes");
        last.finish().expect("finishes");
        for (_request, mut send) in open {
            send.write_all(&not_found()).await.expect("writes");
            send.finish().expect("finishes");
        }

        for request in requests {
            assert_eq!(
                request.await.expect("the request task completes"),
                Ok(ObjectChunk::NotFound)
            );
        }
    })
    .await
    .expect("the test completes well within the timeout");
}
