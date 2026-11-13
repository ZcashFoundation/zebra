//! Loopback tests for [`SyncPeer`], its `get-block-range` client stream, and
//! the registry: a [`SyncPeer`] over a real version 2 connection task, and a
//! raw peer that answers its requests by hand.

use std::{sync::Arc, time::Duration};

use quinn::{Connection, ConnectionError, Endpoint, SendStream};
use tower::service_fn;

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashSerialize},
};

use crate::{
    constants::MAX_PEER_MISBEHAVIOR_SCORE,
    peer::{
        v2::{
            connection::{spawn_connection, ConnectionParams},
            handshake::{CompletedHandshake, Role},
            misbehavior::MisbehaviorScores,
            nonces::NonceRegistry,
            tests::{connected_pair, identity, run_handshake},
        },
        Client,
    },
    peer_set::ActiveConnectionCounter,
    protocol::v2::{
        constants::{MAX_GET_BLOCK_RANGE_BYTES, MAX_GET_BLOCK_RANGE_COUNT},
        request::Request as WireRequest,
        response::block_range::{
            write_block_range_answer, write_block_range_element, BlockRangeAnswer,
        },
        types::StreamType,
    },
    quic::app_error_code,
    BoxError, Response,
};

use super::*;

/// Every loopback exchange finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A [`SyncPeer`] whose connection task runs on one end of a loopback
/// connection, and the raw other end, which answers requests by hand.
struct Fixture {
    peer: SyncPeer,
    /// The raw peer's connection.
    raw: Connection,
    /// The connection task's client, kept so the task keeps running.
    _client: Client,
    /// The raw peer's handshake streams, kept open for the connection's life.
    _raw_handshake: CompletedHandshake,
    _endpoints: (Endpoint, Endpoint),
}

/// Connects a raw peer to a node, completes the handshake, and returns the
/// node's [`SyncPeer`] for the connection. The raw peer dials, so the node
/// does not send it a `get-addr` request.
async fn fixture() -> Fixture {
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

    let services = node_handshake.connection_info.remote.services();
    let remote_tip_height = node_handshake.connection_info.remote.start_height();
    let misbehavior = MisbehaviorScores::spawn(tokio::sync::mpsc::channel(8).0, 8);
    let mut counter = ActiveConnectionCounter::new_counter();
    let (client, closer) = spawn_connection(
        node.clone(),
        node_handshake,
        counter.track_connection(),
        ConnectionParams {
            inbound_service: service_fn(|_| async { Ok::<_, BoxError>(Response::Nil) }),
            inv_collector: tokio::sync::broadcast::channel(16).0,
            genesis_hash: network.genesis_hash(),
            misbehavior: misbehavior.clone(),
            address_book: None,
            discovery: false,
            bans: tokio::sync::watch::channel(crate::BanList::default()).1,
            addr_relay: None,
            serve_block_ranges: true,
            block_range_permits: crate::peer::v2::connection::global_block_range_permits(),
            target_difficulty_limit:
                zebra_chain::work::difficulty::ParameterDifficulty::target_difficulty_limit(
                    &network,
                ),
        },
        None,
    );
    let peer = SyncPeer::new(
        ConnectionId::next(),
        node.remote_address().into(),
        services,
        remote_tip_height,
        node,
        misbehavior,
        closer,
    );

    Fixture {
        peer,
        raw,
        _client: client,
        _raw_handshake: raw_handshake,
        _endpoints: (raw_ep, node_ep),
    }
}

/// Mainnet blocks 2, 1 and 0: a descending chain.
fn chain() -> Vec<Arc<Block>> {
    [
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
    ]
    .into_iter()
    .map(|bytes| Arc::new(Block::zcash_deserialize(bytes).expect("valid vector")))
    .collect()
}

/// The serialized form of each of `blocks`.
fn raw_blocks(blocks: &[Arc<Block>]) -> Vec<Vec<u8>> {
    blocks
        .iter()
        .map(|block| block.zcash_serialize_to_vec().expect("serializes"))
        .collect()
}

/// A request for `count` blocks from `final_hash`, with the largest byte bound.
fn request(final_hash: block::Hash, count: u32) -> GetBlockRange {
    GetBlockRange {
        final_hash,
        count,
        max_bytes: MAX_GET_BLOCK_RANGE_BYTES,
    }
}

/// Accepts the next `get-block-range` stream on the raw peer's connection,
/// and returns its sending half and request. Streams of other types are
/// dropped.
async fn accept_range(raw: &Connection) -> (SendStream, GetBlockRange) {
    loop {
        let (send, mut recv) = raw.accept_bi().await.expect("the connection is open");
        let mut stream_type = [0u8];
        recv.read_exact(&mut stream_type).await.expect("reads");
        if stream_type[0] != StreamType::GetBlockRange.code() {
            continue;
        }
        let body = recv.read_to_end(64).await.expect("reads the request");
        let WireRequest::BlockRange(request) =
            WireRequest::decode(StreamType::GetBlockRange, &body).expect("valid request")
        else {
            panic!("a get-block-range stream carries a get-block-range request");
        };
        return (send, request);
    }
}

/// The bytes of a `Found` answer and `blocks`.
fn found(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut response = Vec::new();
    write_block_range_answer(&mut response, BlockRangeAnswer::Found).expect("writes");
    for block in blocks {
        write_block_range_element(&mut response, block).expect("writes");
    }
    response
}

/// The length of a `Found` response carrying `blocks`.
fn response_len(blocks: &[Vec<u8>]) -> u64 {
    // A test response is small, so the cast is lossless.
    found(blocks).len() as u64
}

/// Answers the next `get-block-range` request with `response`, and finishes
/// the stream.
async fn answer(raw: &Connection, response: Vec<u8>) -> GetBlockRange {
    let (mut send, request) = accept_range(raw).await;
    send.write_all(&response).await.expect("writes");
    send.finish().expect("finishes");
    // Keep the stream until the peer has read it.
    let _ = send.stopped().await;
    request
}

/// The application code `connection` was closed with, once it is closed.
async fn close_code(connection: &Connection) -> u64 {
    match connection.closed().await {
        ConnectionError::ApplicationClosed(close) => close.error_code.into_inner(),
        error => panic!("expected an application close, got {error:?}"),
    }
}

/// A conforming response yields the blocks in descending order, each with
/// its parsed header and hash, then `End` once, then nothing; the permit is
/// released when the stream is dropped.
#[tokio::test]
async fn happy_path_yields_a_descending_range() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        let raw = raw_blocks(&blocks);
        let server = tokio::spawn({
            let connection = fixture.raw.clone();
            let response = found(&raw);
            async move { answer(&connection, response).await }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");
        assert_eq!(fixture.peer.bulk_permits.available_permits(), 1);

        for (block, raw) in blocks.iter().zip(&raw) {
            let item = stream.next().await.expect("an item").expect("a block");
            let RangeItem::Block {
                raw: delivered,
                header,
                hash,
            } = item
            else {
                panic!("expected a block, got {item:?}");
            };
            assert_eq!(hash, block.hash());
            assert_eq!(*header, *block.header);
            assert_eq!(&delivered[..], &raw[..]);
        }
        assert_eq!(
            stream.next().await.expect("an item").expect("the end"),
            RangeItem::End {
                delivered: 3,
                next_expected: blocks[2].header.previous_block_hash,
            },
        );
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none());
        assert_eq!(stream.bytes_received(), response_len(&raw));

        let served = server.await.expect("the server task completes");
        assert_eq!(served, request(blocks[0].hash(), 3));

        drop(stream);
        assert_eq!(
            fixture.peer.bulk_permits.available_permits(),
            BULK_STREAMS_PER_PEER
        );
        assert!(!fixture.peer.is_closed());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A responder that finishes early gives `End` with the hash to resume at,
/// and a request from that hash continues the range.
#[tokio::test]
async fn truncated_range_resumes_at_next_expected() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        let raw = raw_blocks(&blocks);
        let server = tokio::spawn({
            let connection = fixture.raw.clone();
            let (first, rest) = (found(&raw[..1]), found(&raw[1..]));
            async move {
                answer(&connection, first).await;
                answer(&connection, rest).await
            }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");
        let item = stream.next().await.expect("an item").expect("a block");
        assert!(matches!(item, RangeItem::Block { hash, .. } if hash == blocks[0].hash()));
        let end = stream.next().await.expect("an item").expect("the end");
        let RangeItem::End {
            delivered,
            next_expected,
        } = end
        else {
            panic!("expected the end, got {end:?}");
        };
        assert_eq!(delivered, 1);
        assert_eq!(next_expected, blocks[1].hash());
        drop(stream);

        let mut stream = fixture
            .peer
            .get_block_range(request(next_expected, 2))
            .await
            .expect("the stream opens");
        for block in &blocks[1..] {
            let item = stream.next().await.expect("an item").expect("a block");
            assert!(matches!(item, RangeItem::Block { hash, .. } if hash == block.hash()));
        }
        assert!(matches!(
            stream.next().await,
            Some(Ok(RangeItem::End { delivered: 2, .. }))
        ));

        let resumed = server.await.expect("the server task completes");
        assert_eq!(resumed.final_hash, blocks[1].hash());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Dropping a stream before its end cancels it with
/// `STOP_SENDING(CANCELLED)`, which the responder sees, and frees the permit.
#[tokio::test]
async fn dropping_the_stream_stops_sending_with_cancelled() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        let raw = raw_blocks(&blocks);
        let server = tokio::spawn({
            let connection = fixture.raw.clone();
            let response = found(&raw[..1]);
            async move {
                let (mut send, _) = accept_range(&connection).await;
                // One block, and no FIN: the response is still open.
                send.write_all(&response).await.expect("writes");
                send.stopped().await
            }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");
        let item = stream.next().await.expect("an item").expect("a block");
        assert!(matches!(item, RangeItem::Block { .. }));
        drop(stream);
        assert_eq!(
            fixture.peer.bulk_permits.available_permits(),
            BULK_STREAMS_PER_PEER
        );

        let stopped = server.await.expect("the server task completes");
        assert_eq!(
            stopped.expect("the stream is stopped, not lost"),
            Some(app_error_code(ErrorCode::Cancelled)),
        );
        assert!(!fixture.peer.is_closed());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A block that does not hash to the expected hash is a violation: the
/// stream reports it, and the connection task closes the connection with
/// `PROTOCOL_ERROR`.
#[tokio::test]
async fn hash_chain_violation_closes_the_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        let raw = raw_blocks(&blocks);
        tokio::spawn({
            let connection = fixture.raw.clone();
            // Block 2, then genesis: block 1 is skipped.
            let response = found(&[raw[0].clone(), raw[2].clone()]);
            async move { answer(&connection, response).await }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");
        let item = stream.next().await.expect("an item").expect("a block");
        assert!(matches!(item, RangeItem::Block { hash, .. } if hash == blocks[0].hash()));
        let error = stream
            .next()
            .await
            .expect("an item")
            .expect_err("a violation");
        assert!(
            matches!(
                error,
                SyncPeerError::Violation {
                    code: ErrorCode::ProtocolError,
                    ..
                }
            ),
            "got {error:?}"
        );
        assert!(
            fixture.peer.is_closed(),
            "a blamed peer is closing before its connection task closes it"
        );
        assert!(stream.next().await.is_none());

        assert_eq!(
            close_code(&fixture.raw).await,
            ErrorCode::ProtocolError.code()
        );
        tokio::time::timeout(Duration::from_secs(5), fixture.peer.closed())
            .await
            .expect("the peer sees its connection close");
        assert!(fixture.peer.is_closed());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// More blocks than the request's `count` is `FLOOD`, and closes the
/// connection with it.
#[tokio::test]
async fn blocks_past_count_are_flood() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        let raw = raw_blocks(&blocks);
        tokio::spawn({
            let connection = fixture.raw.clone();
            let response = found(&raw);
            async move { answer(&connection, response).await }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 2))
            .await
            .expect("the stream opens");
        for _ in 0..2 {
            assert!(matches!(
                stream.next().await,
                Some(Ok(RangeItem::Block { .. }))
            ));
        }
        let error = stream
            .next()
            .await
            .expect("an item")
            .expect_err("a violation");
        assert!(
            matches!(
                error,
                SyncPeerError::Violation {
                    code: ErrorCode::Flood,
                    ..
                }
            ),
            "got {error:?}"
        );
        assert_eq!(close_code(&fixture.raw).await, ErrorCode::Flood.code());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// `not found` is reported as such, and the connection stays open.
#[tokio::test]
async fn not_found_is_reported() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        tokio::spawn({
            let connection = fixture.raw.clone();
            let mut response = Vec::new();
            write_block_range_answer(&mut response, BlockRangeAnswer::NotFound).expect("writes");
            async move { answer(&connection, response).await }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");
        assert_eq!(
            stream.next().await.expect("an item"),
            Err(SyncPeerError::NotFound)
        );
        assert!(stream.next().await.is_none());
        assert!(!fixture.peer.is_closed());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A reset with `REFUSED` before any data is a refusal, not a transport
/// failure.
#[tokio::test]
async fn refused_reset_is_reported() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        tokio::spawn({
            let connection = fixture.raw.clone();
            async move {
                let (mut send, _) = accept_range(&connection).await;
                send.reset(app_error_code(ErrorCode::Refused))
                    .expect("resets");
                let _ = send.stopped().await;
            }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");
        assert_eq!(
            stream.next().await.expect("an item"),
            Err(SyncPeerError::Refused)
        );
        assert!(!fixture.peer.is_closed());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A reset with `BUSY` or `INTERNAL_ERROR` before any data says the request
/// is worth repeating, unlike `REFUSED`. After a block, the same reset is an
/// ordinary transport failure: the peer was serving the request.
#[tokio::test]
async fn busy_and_internal_error_resets_are_reported_as_busy() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        let raw = raw_blocks(&blocks);

        for code in [ErrorCode::Busy, ErrorCode::InternalError] {
            tokio::spawn({
                let connection = fixture.raw.clone();
                async move {
                    let (mut send, _) = accept_range(&connection).await;
                    send.reset(app_error_code(code)).expect("resets");
                    let _ = send.stopped().await;
                }
            });

            let mut stream = fixture
                .peer
                .get_block_range(request(blocks[0].hash(), 3))
                .await
                .expect("the stream opens");
            assert_eq!(
                stream.next().await.expect("an item"),
                Err(SyncPeerError::Busy),
                "{code:?} before any data",
            );
            assert!(stream.next().await.is_none());
            assert!(!fixture.peer.is_closed());
        }

        let (read_tx, read_rx) = tokio::sync::oneshot::channel();
        tokio::spawn({
            let connection = fixture.raw.clone();
            let response = found(&raw[..1]);
            async move {
                let (mut send, _) = accept_range(&connection).await;
                send.write_all(&response).await.expect("writes");
                // A reset discards unread data, so wait for the block to be read.
                let _ = read_rx.await;
                send.reset(app_error_code(ErrorCode::Busy)).expect("resets");
                let _ = send.stopped().await;
            }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");
        let item = stream.next().await.expect("an item").expect("a block");
        assert!(matches!(item, RangeItem::Block { .. }));
        read_tx.send(()).expect("the responder is waiting");
        assert!(matches!(
            stream.next().await,
            Some(Err(SyncPeerError::Transport(_)))
        ));
        assert!(!fixture.peer.is_closed());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A request above the protocol's bounds is not sent, and takes no permit.
#[tokio::test]
async fn oversized_request_is_not_sent() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let error = fixture
            .peer
            .get_block_range(request(chain()[0].hash(), MAX_GET_BLOCK_RANGE_COUNT + 1))
            .await
            .expect_err("the request is refused locally");
        assert!(
            matches!(error, SyncPeerError::LocalRequest(_)),
            "got {error:?}"
        );
        assert_eq!(
            fixture.peer.bulk_permits.available_permits(),
            BULK_STREAMS_PER_PEER
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// `next` can be abandoned part way through a block and called again
/// without losing data, and `bytes_received` counts the partial block
/// meanwhile.
#[tokio::test]
async fn next_is_cancellation_safe_and_counts_partial_blocks() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let blocks = chain();
        let raw = raw_blocks(&blocks);
        let response = found(&raw[..1]);
        let split = response.len() / 2;
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn({
            let connection = fixture.raw.clone();
            let response = response.clone();
            async move {
                let (mut send, _) = accept_range(&connection).await;
                send.write_all(&response[..split]).await.expect("writes");
                let _ = resume_rx.await;
                send.write_all(&response[split..]).await.expect("writes");
                send.finish().expect("finishes");
                let _ = send.stopped().await;
            }
        });

        let mut stream = fixture
            .peer
            .get_block_range(request(blocks[0].hash(), 3))
            .await
            .expect("the stream opens");

        // A test response is small, so the cast is lossless.
        let split = split as u64;
        while stream.bytes_received() < split {
            assert!(
                tokio::time::timeout(Duration::from_millis(50), stream.next())
                    .await
                    .is_err(),
                "no item is complete yet",
            );
        }
        assert_eq!(stream.bytes_received(), split);

        resume_tx.send(()).expect("the server waits");
        let item = stream.next().await.expect("an item").expect("a block");
        assert!(matches!(item, RangeItem::Block { hash, .. } if hash == blocks[0].hash()));
        assert!(matches!(
            stream.next().await,
            Some(Ok(RangeItem::End { delivered: 1, .. }))
        ));
        assert_eq!(stream.bytes_received(), response_len(&raw[..1]));
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A penalty that reaches the ban threshold closes the connection with
/// `MISBEHAVIOR`.
#[tokio::test]
async fn penalty_at_the_threshold_closes_with_misbehavior() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        fixture
            .peer
            .penalize(MAX_PEER_MISBEHAVIOR_SCORE, "test penalty".to_string())
            .await;
        assert_eq!(
            close_code(&fixture.raw).await,
            ErrorCode::Misbehavior.code()
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// `report_violation` closes the connection with `PROTOCOL_ERROR`.
#[tokio::test]
async fn reported_violation_closes_with_protocol_error() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        fixture.peer.report_violation("bad merkle root".to_string());
        assert!(fixture.peer.is_closed());
        assert_eq!(
            close_code(&fixture.raw).await,
            ErrorCode::ProtocolError.code()
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// The registry publishes a registered connection, removes it when the
/// connection closes, and exits with its last sender.
#[tokio::test]
async fn registry_removes_a_closed_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = fixture().await;
        let (sender, mut peers) = SyncPeersSender::spawn();
        assert!(peers.borrow().is_empty());
        assert_eq!(sender, sender.clone());
        assert_ne!(sender, SyncPeersSender::spawn().0);

        let id = fixture.peer.id();
        sender.connected(fixture.peer.clone());
        let published = peers
            .wait_for(|peers| !peers.is_empty())
            .await
            .expect("the registry runs")
            .clone();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].id(), id);
        drop(published);

        fixture.raw.close(app_error_code(ErrorCode::NoError), b"");
        peers
            .wait_for(|peers| peers.is_empty())
            .await
            .expect("the registry runs");

        drop(sender);
        peers
            .changed()
            .await
            .expect_err("the registry exits with its last sender");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// `check_block` accepts a block that hashes to the expected hash, and
/// rejects one over `MAX_BLOCK_BYTES`, one with a malformed header, and one
/// with the wrong hash.
#[test]
fn check_block_rejects_oversized_malformed_and_unexpected_blocks() {
    let _init_guard = zebra_test::init();

    let blocks = chain();
    let raw = raw_blocks(&blocks);

    let header = block_range::check_block(&raw[0], blocks[0].hash()).expect("the expected block");
    assert_eq!(block::Hash::from(&header), blocks[0].hash());

    // `MAX_BLOCK_BYTES` is below the 2 MiB element limit, so a peer can send this.
    let oversized = vec![
        0;
        usize::try_from(block::MAX_BLOCK_BYTES)
            .expect("MAX_BLOCK_BYTES is 2 MB, which fits in a usize")
            + 1
    ];
    let error =
        block_range::check_block(&oversized, blocks[0].hash()).expect_err("an oversized block");
    assert!(error.contains("block limit"), "got {error}");

    for malformed in [&raw[0][..10], &[0xff; 200][..]] {
        let error =
            block_range::check_block(malformed, blocks[0].hash()).expect_err("a malformed header");
        assert!(error.contains("malformed header"), "got {error}");
    }

    let error =
        block_range::check_block(&raw[1], blocks[0].hash()).expect_err("an unexpected block");
    assert!(error.contains("where the chain needs"), "got {error}");
}

/// The registry removes a connection whose connection task was aborted,
/// which leaves the connection open while handles remain, and closes it with
/// `NO_ERROR`.
#[tokio::test]
async fn registry_closes_and_removes_a_connection_whose_task_was_aborted() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let Fixture {
            peer,
            raw,
            _client: client,
            _raw_handshake,
            _endpoints,
        } = fixture().await;
        let (sender, mut peers) = SyncPeersSender::spawn();
        sender.connected(peer.clone());
        peers
            .wait_for(|peers| !peers.is_empty())
            .await
            .expect("the registry runs");

        // Dropping the client aborts the connection task, which does not close its connection.
        drop(client);
        peers
            .wait_for(|peers| peers.is_empty())
            .await
            .expect("the registry runs");
        assert!(peer.is_closed());
        assert_eq!(close_code(&raw).await, ErrorCode::NoError.code());
    })
    .await
    .expect("the test completes well within the timeout");
}
