//! Loopback tests for the version 2 connection: two nodes connected over
//! QUIC, each presenting the other as a [`Client`].

use std::{collections::HashSet, net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use futures::StreamExt;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use tokio::{sync::broadcast, task::JoinSet, time::Instant};
use tower::{service_fn, util::BoxCloneService, Service, ServiceExt};

use zebra_chain::{
    block::{self, Block, CountedHeader},
    parameters::Network,
    serialization::{DateTime32, ZcashDeserialize, ZcashSerialize},
    transaction::{self, Transaction, UnminedTx, UnminedTxId},
};

use crate::{
    constants::REQUEST_TIMEOUT,
    meta_addr::MetaAddr,
    peer::{
        v2::{
            connection::{
                announcements::AnnouncementWriter,
                inbound::{INBOUND_REQUEST_CONCURRENCY, RESPONSE_BYTES_LIMIT},
                outbound::{RequestSend, MAX_OUTBOUND_REQUEST_STREAMS},
                spawn_connection, ConnectionParams,
            },
            handshake::{CompletedHandshake, Role},
            nonces::NonceRegistry,
            tests::{connected_pair, identity, run_handshake},
        },
        Client,
    },
    peer_set::{ActiveConnectionCounter, InventoryChange},
    protocol::{
        external::{types::PeerServices, InventoryHash},
        internal::{InventoryResponse, Request, Response},
        v2::{
            announcement::{encode_tx_record, BLOCK_ANNOUNCEMENT_HEADER},
            handshake::{InitRecord, INIT_RECORD_KIND},
            record::RecordCodec,
            request::{GetBlocks, GetHeaders, GetTx, Request as WireRequest},
            response::{
                decode_addr_response, encode_addr_response, read_headers, read_objects,
                write_headers_response, ObjectResult,
            },
            txref::{ShortTxId, TxRef},
            types::{ErrorCode, StreamType},
        },
    },
    quic::app_error_code,
    BoxError, PeerSocketAddr, MAX_TX_INV_IN_SENT_MESSAGE,
};

/// Every loopback exchange finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a test waits to see that something does not happen.
const SHORT_WAIT: Duration = Duration::from_millis(500);

/// A node for the tests: a connection task fed by an inbound service that
/// answers from fixed data.
struct TestNode {
    client: Client,
    inv_rx: broadcast::Receiver<InventoryChange>,
    inbound_rx: tokio::sync::mpsc::UnboundedReceiver<Request>,
}

/// Fixed data the test inbound service answers with.
#[derive(Clone)]
struct Fixture {
    blocks: Vec<Arc<Block>>,
    addrs: Vec<MetaAddr>,
}

impl Fixture {
    fn new() -> Self {
        let blocks = [
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
        ]
        .into_iter()
        .map(|bytes| Arc::new(Block::zcash_deserialize(bytes).expect("valid vector")))
        .collect();
        let addr: PeerSocketAddr = "203.0.113.5:8233".parse::<SocketAddr>().unwrap().into();
        let addrs = vec![MetaAddr::new_gossiped_meta_addr(
            addr,
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        )];
        Fixture { blocks, addrs }
    }

    /// Answers a request from the fixture, like a tiny state service.
    fn answer(&self, request: Request) -> Response {
        match request {
            Request::Peers => Response::Peers(self.addrs.clone()),
            Request::BlocksByHash(hashes) => Response::Blocks(
                hashes
                    .into_iter()
                    .map(|hash| match self.blocks.iter().find(|b| b.hash() == hash) {
                        Some(block) => InventoryResponse::Available((block.clone(), None)),
                        None => InventoryResponse::Missing(hash),
                    })
                    .collect(),
            ),
            Request::TransactionsById(ids) => Response::Transactions(
                ids.into_iter()
                    .map(|id| {
                        let found = self
                            .blocks
                            .iter()
                            .flat_map(|b| b.transactions.iter())
                            .find(|tx| UnminedTxId::from(tx.as_ref().clone()) == id);
                        match found {
                            Some(tx) => {
                                InventoryResponse::Available((UnminedTx::from(tx.clone()), None))
                            }
                            None => InventoryResponse::Missing(id),
                        }
                    })
                    .collect(),
            ),
            Request::FindHeaders { known_blocks, .. } => {
                // Headers after the first known block, or, like the state, from the genesis
                // block if none is known.
                let start = known_blocks
                    .iter()
                    .find_map(|known| self.blocks.iter().position(|b| b.hash() == *known))
                    .map(|index| index + 1)
                    .unwrap_or(0);
                Response::BlockHeaders(
                    self.blocks[start.min(self.blocks.len())..]
                        .iter()
                        .map(|b| CountedHeader {
                            header: b.header.clone(),
                        })
                        .collect(),
                )
            }
            Request::MempoolTransactionIds => Response::TransactionIds(
                self.blocks[1..]
                    .iter()
                    .map(|b| UnminedTxId::from(b.transactions[0].as_ref().clone()))
                    .collect(),
            ),
            _ => Response::Nil,
        }
    }
}

/// An inbound service for the tests.
type TestService = BoxCloneService<Request, Response, BoxError>;

/// Spawns a connection task over `connection` with the fixture as its
/// inbound service.
fn spawn_node(connection: Connection, completed: CompletedHandshake, fixture: Fixture) -> TestNode {
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel();
    let inbound_service = service_fn(move |request: Request| {
        let fixture = fixture.clone();
        let inbound_tx = inbound_tx.clone();
        async move {
            let _ = inbound_tx.send(request.clone());
            Ok::<Response, BoxError>(fixture.answer(request))
        }
    });
    spawn_node_with(
        connection,
        completed,
        BoxCloneService::new(inbound_service),
        inbound_rx,
    )
}

/// Spawns a connection task over `connection` with `inbound_service`; its
/// requests are only seen on `inbound_rx` if the service sends them there.
fn spawn_node_with(
    connection: Connection,
    completed: CompletedHandshake,
    inbound_service: TestService,
    inbound_rx: tokio::sync::mpsc::UnboundedReceiver<Request>,
) -> TestNode {
    let (inv_tx, inv_rx) = broadcast::channel(16);
    let mut counter = ActiveConnectionCounter::new_counter();

    let client = spawn_connection(
        connection,
        completed,
        counter.track_connection(),
        ConnectionParams {
            inbound_service,
            inv_collector: inv_tx,
        },
        None,
    );
    TestNode {
        client,
        inv_rx,
        inbound_rx,
    }
}

/// Two connected nodes: the first dialled the second.
async fn connected_nodes(network: &Network) -> (TestNode, TestNode, Endpoint, Endpoint) {
    let (a, b, _connections, client_ep, server_ep) =
        connected_nodes_with_connections(network).await;
    (a, b, client_ep, server_ep)
}

/// Two connected nodes, the first dialled the second, with a handle to each
/// one's QUIC connection, for opening raw streams beside the connection
/// tasks.
async fn connected_nodes_with_connections(
    network: &Network,
) -> (
    TestNode,
    TestNode,
    (Connection, Connection),
    Endpoint,
    Endpoint,
) {
    let (outbound, inbound, client_ep, server_ep) = connected_pair(network).await;
    let connections = (outbound.clone(), inbound.clone());
    let (a, b) = (identity("/a/"), identity("/b/"));
    let (a_nonces, b_nonces) = (NonceRegistry::spawn(8), NonceRegistry::spawn(8));
    let (initiator, responder) = tokio::join!(
        run_handshake(&outbound, Role::Initiator, network, &a, &a_nonces),
        run_handshake(&inbound, Role::Responder, network, &b, &b_nonces),
    );
    let initiator = initiator.expect("the handshake completes");
    let responder = responder.expect("the handshake completes");
    let fixture = Fixture::new();
    (
        spawn_node(outbound, initiator, fixture.clone()),
        spawn_node(inbound, responder, fixture),
        connections,
        client_ep,
        server_ep,
    )
}

/// A node connected to a raw peer that completed the handshake by hand, with
/// the raw side's handshake stream and `init` record for driving violations.
struct RawPeer {
    connection: Connection,
    hs_send: SendStream,
    _hs_recv: RecvStream,
    init_record: Bytes,
    node: TestNode,
    _endpoints: (Endpoint, Endpoint),
}

/// Connects a raw peer to a node serving the fixture and completes the
/// handshake by hand. The raw peer dials the node.
async fn raw_peer_node(network: &Network) -> RawPeer {
    raw_peer_node_with(network, None).await
}

/// Connects a raw peer to a node and completes the handshake by hand. The
/// node serves `inbound_service`, or the fixture if `None`.
async fn raw_peer_node_with(network: &Network, inbound_service: Option<TestService>) -> RawPeer {
    raw_peer_node_relaying(network, inbound_service, true).await
}

/// Connects a raw peer to a node and completes the handshake by hand, with
/// `relay` in the raw peer's `init` record. The node serves
/// `inbound_service`, or the fixture if `None`.
async fn raw_peer_node_relaying(
    network: &Network,
    inbound_service: Option<TestService>,
    relay: bool,
) -> RawPeer {
    let (connection, inbound, client_ep, server_ep) = connected_pair(network).await;
    let raw_init = InitRecord {
        version: crate::constants::CURRENT_NETWORK_PROTOCOL_VERSION,
        services: PeerServices::NODE_NETWORK,
        nonce: Default::default(),
        user_agent: "/raw/".to_string(),
        start_height: block::Height(0),
        relay,
        announce: false,
        full_ids: false,
    };

    let (mut hs_send, hs_recv) = connection.open_bi().await.expect("opens");
    hs_send
        .write_all(&[StreamType::Handshake.code()])
        .await
        .expect("writes");
    let init_record = RecordCodec::encode_record(raw_init.encode()).expect("valid");
    hs_send.write_all(&init_record).await.expect("writes");
    let (b, nonces) = (identity("/b/"), NonceRegistry::spawn(8));
    let (completed, hs_recv) = tokio::join!(
        run_handshake(&inbound, Role::Responder, network, &b, &nonces),
        async {
            let mut framed = tokio_util::codec::FramedRead::new(hs_recv, RecordCodec);
            let payload = framed.next().await.expect("init").expect("valid");
            assert_eq!(payload[0], INIT_RECORD_KIND);
            framed.into_inner()
        }
    );
    let completed = completed.expect("the handshake completes");
    let node = match inbound_service {
        Some(service) => spawn_node_with(
            inbound,
            completed,
            service,
            tokio::sync::mpsc::unbounded_channel().1,
        ),
        None => spawn_node(inbound, completed, Fixture::new()),
    };

    RawPeer {
        connection,
        hs_send,
        _hs_recv: hs_recv,
        init_record,
        node,
        _endpoints: (client_ep, server_ep),
    }
}

/// Sends `request` through `client`.
async fn call(
    client: &mut Client,
    request: Request,
) -> Result<Response, crate::peer::SharedPeerError> {
    client.ready().await?;
    client.call(request).await
}

/// Requests of every kind flow in both directions and come back as the
/// legacy responses the peer set expects.
#[tokio::test]
async fn requests_round_trip_between_two_nodes() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (mut a, mut b, _e1, _e2) = connected_nodes(&network).await;
        let fixture = Fixture::new();
        let genesis = fixture.blocks[0].clone();
        let block_1 = fixture.blocks[1].clone();
        let block_2 = fixture.blocks[2].clone();

        // Peers: served from B's inbound service.
        let response = call(&mut a.client, Request::Peers).await.expect("peers");
        match response {
            Response::Peers(addrs) => {
                assert_eq!(addrs.len(), 1);
                assert_eq!(addrs[0].addr(), fixture.addrs[0].addr());
            }
            other => panic!("unexpected response {other}"),
        }
        assert!(matches!(b.inbound_rx.recv().await, Some(Request::Peers)));

        // Blocks: found and missing, in request order, from the other direction too.
        let missing = block::Hash([0xAB; 32]);
        let hashes: HashSet<_> = [block_1.hash(), missing].into_iter().collect();
        let response = call(&mut b.client, Request::BlocksByHash(hashes))
            .await
            .expect("blocks");
        match response {
            Response::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert!(blocks.iter().any(|b| matches!(b, InventoryResponse::Available((block, Some(_))) if block.hash() == block_1.hash())));
                assert!(blocks.iter().any(|b| matches!(b, InventoryResponse::Missing(hash) if *hash == missing)));
            }
            other => panic!("unexpected response {other}"),
        }
        assert!(matches!(a.inbound_rx.recv().await, Some(Request::BlocksByHash(_))));

        // A single block download.
        let response = call(
            &mut a.client,
            Request::BlockDownload {
                hash: block_2.hash(),
                advertiser: None,
            },
        )
        .await
        .expect("block");
        assert!(matches!(response, Response::Blocks(blocks) if blocks.len() == 1));

        // Transactions: the coinbase of block 1 by legacy ID, plus one unknown.
        let coinbase_id = UnminedTxId::from(block_1.transactions[0].as_ref().clone());
        let unknown_id = UnminedTxId::Legacy(zebra_chain::transaction::Hash([0xCD; 32]));
        let response = call(
            &mut a.client,
            Request::TransactionsById([coinbase_id, unknown_id].into_iter().collect()),
        )
        .await
        .expect("transactions");
        match response {
            Response::Transactions(txs) => {
                assert_eq!(txs.len(), 2);
                assert!(txs.iter().any(|t| matches!(t, InventoryResponse::Available((tx, _)) if tx.id == coinbase_id)));
                assert!(txs.iter().any(|t| matches!(t, InventoryResponse::Missing(id) if *id == unknown_id)));
            }
            other => panic!("unexpected response {other}"),
        }

        // Headers and hashes after genesis.
        let response = call(
            &mut a.client,
            Request::FindHeaders {
                known_blocks: vec![genesis.hash()],
                stop: None,
            },
        )
        .await
        .expect("headers");
        match response {
            Response::BlockHeaders(headers) => {
                assert_eq!(headers.len(), 2);
                assert_eq!(headers[0].header.hash(), block_1.hash());
                assert_eq!(headers[1].header.hash(), block_2.hash());
            }
            other => panic!("unexpected response {other}"),
        }
        let response = call(
            &mut a.client,
            Request::FindBlocks {
                known_blocks: vec![genesis.hash()],
                stop: None,
            },
        )
        .await
        .expect("hashes");
        assert_eq!(
            response,
            Response::BlockHashes(vec![block_1.hash(), block_2.hash()])
        );

        // Ping is answered from the transport's round-trip time.
        let response = call(&mut a.client, Request::Ping(Default::default()))
            .await
            .expect("pong");
        assert!(matches!(response, Response::Pong(_)));
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Transaction announcements reach the other node's inbound service and
/// inventory registry. Inbound address and block announcements are covered
/// by `inbound_announcements_reach_the_node`.
#[tokio::test]
async fn announcements_flow_between_two_nodes() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (mut a, mut b, _e1, _e2) = connected_nodes(&network).await;
        let fixture = Fixture::new();
        let id = UnminedTxId::from(fixture.blocks[1].transactions[0].as_ref().clone());

        let response = call(
            &mut a.client,
            Request::AdvertiseTransactionIds([id].into_iter().collect(), None),
        )
        .await
        .expect("advertise");
        assert_eq!(response, Response::Nil);

        // B's inbound service sees the advertisement tagged with A's address.
        let request = b.inbound_rx.recv().await.expect("a request arrives");
        match request {
            Request::AdvertiseTransactionIds(ids, Some(addr)) => {
                assert_eq!(ids, [id].into_iter().collect());
                assert_eq!(addr, b.client.connection_info.connected_addr.get_transient_addr().unwrap());
            }
            other => panic!("unexpected request {other}"),
        }
        // And B's inventory registry sees it as available at A.
        let change = b.inv_rx.recv().await.expect("an inventory change");
        assert!(matches!(change, InventoryChange::Available((hashes, _)) if hashes.first() == &InventoryHash::from(id)));

        // Pushing a transaction announces its ID too.
        let tx = UnminedTx::from(fixture.blocks[2].transactions[0].clone());
        call(&mut a.client, Request::PushTransaction(tx.clone(), None))
            .await
            .expect("push");
        let request = b.inbound_rx.recv().await.expect("a request arrives");
        assert!(matches!(request, Request::AdvertiseTransactionIds(ids, _) if ids.contains(&tx.id)));

        // Block advertisements need a header and are not sent yet.
        let response = call(&mut a.client, Request::AdvertiseBlock(block::Hash([1; 32]), None))
            .await
            .expect("advertise block");
        assert_eq!(response, Response::Nil);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Dropping one side's client ends the connection cleanly on the other.
#[tokio::test]
async fn dropping_a_client_closes_the_connection() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (a, mut b, _e1, _e2) = connected_nodes(&network).await;
        drop(a);

        // B's client fails on its next use.
        let error = loop {
            match call(&mut b.client, Request::Ping(Default::default())).await {
                Ok(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(error) => break error,
            }
        };
        let inner = error.inner_debug();
        assert!(
            inner.contains("ConnectionClosed")
                || inner.contains("ConnectionTaskExited")
                || inner.contains("ConnectionDropped"),
            "got {inner}"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Protocol violations on a live connection close it with the draft's code,
/// and the client reports the matching error.
///
/// Not covered here: a request stream that waits for a permit past
/// `REQUEST_TIMEOUT` (refused with `REFUSED`), which
/// `stalled_requests_end_at_the_request_timeout` covers; and a request that
/// decodes to misbehavior rather than a violation, which no request format
/// can produce (misbehavior is a response-side rule).
#[tokio::test]
async fn violations_close_the_connection() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        for violation in [
            "second init",
            "unknown record kind then second init",
            "malformed second init",
            "handshake stream flood after handshake",
            "handshake stream finished after handshake",
            "second handshake stream",
            "duplicate announcement stream",
            "nonconforming announcement record",
            "oversized announcement prefix",
            "bi stream finished before type byte",
            "uni stream finished before type byte",
            "oversize request",
            "malformed request",
            "unknown stream type",
        ] {
            let mut raw = raw_peer_node(&network).await;

            // The close code, and what the client's error names.
            let expected = match violation {
                "second init" => {
                    raw.hs_send
                        .write_all(&raw.init_record)
                        .await
                        .expect("writes");
                    Some((ErrorCode::ProtocolError, "DuplicateHandshake"))
                }
                "unknown record kind then second init" => {
                    // The unknown kind is ignored; the init behind it on the same stream is the
                    // violation, which the client's error names.
                    let unknown = RecordCodec::encode_record([0x07, 0xDE, 0xAD]).expect("valid");
                    raw.hs_send.write_all(&unknown).await.expect("writes");
                    raw.hs_send
                        .write_all(&raw.init_record)
                        .await
                        .expect("writes");
                    Some((ErrorCode::ProtocolError, "DuplicateHandshake"))
                }
                "malformed second init" => {
                    let malformed =
                        RecordCodec::encode_record([INIT_RECORD_KIND, 1, 2, 3]).expect("valid");
                    raw.hs_send.write_all(&malformed).await.expect("writes");
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "handshake stream flood after handshake" => {
                    // A record length prefix above the payload limit.
                    raw.hs_send
                        .write_all(&u32::MAX.to_le_bytes())
                        .await
                        .expect("writes");
                    Some((ErrorCode::Flood, "ProtocolViolation"))
                }
                "handshake stream finished after handshake" => {
                    // The peer intends to disconnect: a clean close.
                    raw.hs_send.finish().expect("finishes");
                    Some((ErrorCode::NoError, "ConnectionClosed"))
                }
                "second handshake stream" => {
                    let (mut send, _recv) = raw.connection.open_bi().await.expect("opens");
                    send.write_all(&[StreamType::Handshake.code()])
                        .await
                        .expect("writes");
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "duplicate announcement stream" => {
                    // Two concurrent streams of a type are tolerated for the replacement window;
                    // a third waits for a reader to finish, which these never do, and is then the
                    // violation.
                    let mut streams = Vec::new();
                    for _ in 0..3 {
                        let mut stream = raw.connection.open_uni().await.expect("opens");
                        stream
                            .write_all(&[StreamType::TransactionAnnouncements.code()])
                            .await
                            .expect("writes");
                        streams.push(stream);
                    }
                    std::mem::forget(streams);
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "nonconforming announcement record" => {
                    // A transaction announcement whose reference type byte is undefined.
                    let mut send = raw.connection.open_uni().await.expect("opens");
                    send.write_all(&[StreamType::TransactionAnnouncements.code()])
                        .await
                        .expect("writes");
                    let record = RecordCodec::encode_record([0xFF]).expect("valid");
                    send.write_all(&record).await.expect("writes");
                    std::mem::forget(send);
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "oversized announcement prefix" => {
                    // A record length prefix above the payload limit.
                    let mut send = raw.connection.open_uni().await.expect("opens");
                    send.write_all(&[StreamType::AddressAnnouncements.code()])
                        .await
                        .expect("writes");
                    send.write_all(&u32::MAX.to_le_bytes())
                        .await
                        .expect("writes");
                    std::mem::forget(send);
                    Some((ErrorCode::Flood, "ProtocolViolation"))
                }
                "bi stream finished before type byte" => {
                    let (mut send, _recv) = raw.connection.open_bi().await.expect("opens");
                    send.finish().expect("finishes");
                    std::mem::forget(send);
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "uni stream finished before type byte" => {
                    let mut send = raw.connection.open_uni().await.expect("opens");
                    send.finish().expect("finishes");
                    std::mem::forget(send);
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "oversize request" => {
                    // A get-addr request has an empty body.
                    let (mut send, _recv) = raw.connection.open_bi().await.expect("opens");
                    send.write_all(&[StreamType::GetAddr.code(), 0])
                        .await
                        .expect("writes");
                    send.finish().expect("finishes");
                    std::mem::forget(send);
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "malformed request" => {
                    // A get-blocks request whose count claims two hashes but has one.
                    let (mut send, _recv) = raw.connection.open_bi().await.expect("opens");
                    let mut request = vec![StreamType::GetBlocks.code(), 2];
                    request.extend_from_slice(&[0x11; 32]);
                    send.write_all(&request).await.expect("writes");
                    send.finish().expect("finishes");
                    std::mem::forget(send);
                    Some((ErrorCode::ProtocolError, "ProtocolViolation"))
                }
                "unknown stream type" => {
                    // Refused per stream; the connection stays open.
                    let (mut send, mut recv) = raw.connection.open_bi().await.expect("opens");
                    send.write_all(&[0x7E]).await.expect("writes");
                    send.finish().expect("finishes");
                    assert_reset(recv.read_to_end(16).await, ErrorCode::UnsupportedStreamType);
                    assert!(
                        raw.connection.close_reason().is_none(),
                        "the connection stays open"
                    );
                    None
                }
                _ => unreachable!(),
            };

            if let Some((expected_code, expected_error)) = expected {
                // Some cases close only after the node's request timeout.
                let close = tokio::time::timeout(TEST_TIMEOUT / 2, raw.connection.closed())
                    .await
                    .unwrap_or_else(|_| panic!("{violation}: the connection stays open"));
                match close {
                    quinn::ConnectionError::ApplicationClosed(close) => {
                        assert_eq!(
                            close.error_code.into_inner(),
                            expected_code.code(),
                            "{violation}"
                        );
                    }
                    other => panic!("{violation}: unexpected close {other:?}"),
                }
                // The client reports the failure.
                let error = call(&mut raw.node.client, Request::Ping(Default::default()))
                    .await
                    .expect_err("the connection failed");
                assert!(
                    error.inner_debug().contains(expected_error),
                    "{violation}: expected {expected_error}, got {}",
                    error.inner_debug()
                );
            }
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Asserts that a read of a request stream's response failed because the
/// node reset the stream with `code`.
fn assert_reset<T: std::fmt::Debug>(read: Result<T, quinn::ReadToEndError>, code: ErrorCode) {
    let error = read.expect_err("the stream is reset");
    assert!(
        matches!(error, quinn::ReadToEndError::Read(quinn::ReadError::Reset(reset))
            if reset.into_inner() == code.code()),
        "expected a {code:?} reset, got {error:?}"
    );
}

/// A request the inbound service fails to answer is refused with `REFUSED`,
/// and the connection stays open.
#[tokio::test]
async fn failed_requests_are_refused() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let failing = service_fn(|_request: Request| async {
            Err::<Response, BoxError>("the inbound service failed".into())
        });
        let raw = raw_peer_node_with(&network, Some(BoxCloneService::new(failing))).await;

        let mut recv = raw_request(&raw.connection, &WireRequest::Addr).await;
        assert_reset(recv.read_to_end(1024).await, ErrorCode::Refused);
        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Sends `request` on a new request stream and returns the response side.
async fn raw_request(connection: &Connection, request: &WireRequest) -> RecvStream {
    let (mut send, recv) = connection.open_bi().await.expect("opens");
    send.write_all(&[request.stream_type().code()])
        .await
        .expect("writes");
    send.write_all(&request.encode().expect("encodes"))
        .await
        .expect("writes");
    send.finish().expect("finishes");
    recv
}

/// The blocks of the mainnet test vectors and one large testnet block.
fn vector_blocks() -> Vec<Arc<Block>> {
    zebra_test::vectors::MAINNET_BLOCKS
        .values()
        .copied()
        .chain([&zebra_test::vectors::BLOCK_TESTNET_141042_BYTES[..]])
        .map(|bytes| Arc::new(Block::zcash_deserialize(bytes).expect("valid vector")))
        .collect()
}

/// An inbound service that knows `blocks` and their transactions, and
/// answers a block or transaction lookup with at most `max_count` of the
/// requested items, taken in `HashSet` order and answered in reverse, as an
/// inbound service with response limits may: the rest are left out, not
/// reported missing.
fn subset_service(blocks: Vec<Arc<Block>>, max_count: usize) -> TestService {
    let blocks = Arc::new(blocks);
    BoxCloneService::new(service_fn(move |request: Request| {
        let blocks = blocks.clone();
        async move {
            let response = match request {
                Request::BlocksByHash(hashes) => {
                    let mut answers: Vec<_> = hashes
                        .into_iter()
                        .take(max_count)
                        .map(|hash| match blocks.iter().find(|b| b.hash() == hash) {
                            Some(block) => InventoryResponse::Available((block.clone(), None)),
                            None => InventoryResponse::Missing(hash),
                        })
                        .collect();
                    answers.reverse();
                    Response::Blocks(answers)
                }
                Request::TransactionsById(ids) => {
                    let mut answers: Vec<_> = ids
                        .into_iter()
                        .take(max_count)
                        .map(|id| {
                            let found = blocks
                                .iter()
                                .flat_map(|b| b.transactions.iter())
                                .find(|tx| UnminedTxId::from(tx.as_ref().clone()) == id);
                            match found {
                                Some(tx) => InventoryResponse::Available((
                                    UnminedTx::from(tx.clone()),
                                    None,
                                )),
                                None => InventoryResponse::Missing(id),
                            }
                        })
                        .collect();
                    answers.reverse();
                    Response::Transactions(answers)
                }
                _ => Response::Nil,
            };
            Ok::<Response, BoxError>(response)
        }
    }))
}

/// The number of entries of the given lengths a response holds under the
/// byte budget: the budget is checked before each entry.
fn budget_prefix(entry_lens: impl IntoIterator<Item = usize>) -> usize {
    let (mut sent, mut count) = (0, 0);
    for len in entry_lens {
        if sent >= RESPONSE_BYTES_LIMIT {
            break;
        }
        sent += len;
        count += 1;
    }
    count
}

/// The length of the response entry for `object`.
fn entry_len<T: ZcashSerialize>(object: &Arc<T>) -> usize {
    let mut bytes = Vec::new();
    ObjectResult::Found(object.clone())
        .write(&mut bytes)
        .expect("writes to a Vec");
    bytes.len()
}

/// A `get-blocks` response answers every hash in request order, whichever
/// subset the inbound service answers each lookup with, up to the byte
/// budget, and a repeated hash counts against the budget each time.
#[tokio::test]
async fn get_blocks_answers_in_request_order_within_the_byte_budget() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let blocks = vector_blocks();
        let raw = raw_peer_node_with(&network, Some(subset_service(blocks.clone(), 3))).await;

        // Distinct blocks, small enough together to fit the budget, and one unknown hash.
        let mut known: Vec<_> = blocks
            .iter()
            .filter(|block| entry_len(block) < 50_000)
            .take(20)
            .cloned()
            .collect();
        known.reverse();
        assert_eq!(known.len(), 20, "enough small vector blocks");
        let unknown = block::Hash([0xAB; 32]);
        let mut hashes: Vec<_> = known.iter().map(|block| block.hash()).collect();
        hashes.insert(7, unknown);
        assert!(known.iter().map(entry_len).sum::<usize>() < RESPONSE_BYTES_LIMIT);

        let request = WireRequest::Blocks(GetBlocks {
            hashes: hashes.clone(),
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, hashes.len())
            .await
            .expect("a well-formed response");
        assert_eq!(entries.len(), hashes.len(), "every hash is answered");
        for (entry, hash) in entries.iter().zip(&hashes) {
            match entry {
                ObjectResult::Found(block) => assert_eq!(block.hash(), *hash),
                ObjectResult::NotFound => assert_eq!(*hash, unknown),
            }
        }

        // Many copies of one block: answered from the first lookup, until the budget.
        let block = blocks
            .iter()
            .max_by_key(|block| {
                let len = entry_len(block);
                // The largest block well under the budget.
                if len < RESPONSE_BYTES_LIMIT / 10 {
                    len
                } else {
                    0
                }
            })
            .expect("vector blocks")
            .clone();
        let copies = crate::protocol::v2::constants::MAX_BLOCKS_PER_REQUEST;
        let expected = budget_prefix(std::iter::repeat_n(entry_len(&block), copies));
        assert!(expected < copies, "the budget cuts the response short");
        let request = WireRequest::Blocks(GetBlocks {
            hashes: vec![block.hash(); copies],
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, copies)
            .await
            .expect("a well-formed response that finishes early");
        assert_eq!(entries.len(), expected);
        assert!(entries.iter().all(
            |entry| matches!(entry, ObjectResult::Found(found) if found.hash() == block.hash())
        ));

        // A block larger than the budget is sent, but on its own.
        let large = blocks
            .iter()
            .max_by_key(|block| entry_len(block))
            .expect("vector blocks")
            .clone();
        assert!(entry_len(&large) > RESPONSE_BYTES_LIMIT);
        let request = WireRequest::Blocks(GetBlocks {
            hashes: vec![large.hash(), known[0].hash()],
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, 2)
            .await
            .expect("a well-formed response that finishes early");
        assert!(
            matches!(&entries[..], [ObjectResult::Found(found)] if found.hash() == large.hash())
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A lookup that fails after some entries were written finishes the
/// response early, so the requester keeps those entries, instead of
/// resetting the stream; a lookup that fails before any entry still refuses
/// the stream.
#[tokio::test]
async fn failed_lookups_part_way_finish_the_response_early() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let blocks = vector_blocks();
        let first = blocks[0].clone();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // Answers the first lookup of each response, then fails every later one, like an
        // inbound service that starts shedding load part way through.
        let service: TestService = {
            let first = first.clone();
            let calls = calls.clone();
            BoxCloneService::new(service_fn(move |request: Request| {
                let first = first.clone();
                let calls = calls.clone();
                async move {
                    match request {
                        Request::BlocksByHash(hashes)
                            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 =>
                        {
                            Ok(Response::Blocks(
                                hashes
                                    .into_iter()
                                    .filter(|hash| *hash == first.hash())
                                    .map(|_| InventoryResponse::Available((first.clone(), None)))
                                    .collect(),
                            ))
                        }
                        _ => Err::<Response, BoxError>("overloaded".into()),
                    }
                }
            }))
        };
        let raw = raw_peer_node_with(&network, Some(service)).await;

        let hashes = vec![first.hash(), blocks[1].hash(), blocks[2].hash()];
        let request = WireRequest::Blocks(GetBlocks {
            hashes: hashes.clone(),
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, hashes.len())
            .await
            .expect("a response that finishes early, not a reset stream");
        assert!(
            matches!(&entries[..], [ObjectResult::Found(found)] if found.hash() == first.hash())
        );

        // Every lookup now fails, so nothing is written and the stream is refused.
        let request = WireRequest::Blocks(GetBlocks {
            hashes: vec![blocks[1].hash()],
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        read_objects::<_, Block>(&mut recv, 1)
            .await
            .expect_err("a refused stream");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `get-tx` response answers every reference in request order, whichever
/// subset the inbound service answers each lookup with, up to the byte
/// budget; short IDs are not found, and a repeated reference counts against
/// the budget each time.
#[tokio::test]
async fn get_tx_answers_in_request_order_within_the_byte_budget() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let blocks = vector_blocks();
        let raw = raw_peer_node_with(&network, Some(subset_service(blocks.clone(), 3))).await;

        // Distinct transactions, small enough to fit the budget, each followed by unknown IDs, so
        // the request spans more than one lookup chunk.
        let mut transactions: Vec<Arc<Transaction>> = Vec::new();
        for transaction in blocks.iter().flat_map(|block| block.transactions.iter()) {
            let id = UnminedTxId::from(transaction.as_ref().clone());
            if entry_len(transaction) < 2_000
                && !transactions
                    .iter()
                    .any(|known| UnminedTxId::from(known.as_ref().clone()) == id)
            {
                transactions.push(transaction.clone());
            }
        }
        transactions.truncate(60);
        assert!(transactions.len() >= 20, "enough small vector transactions");
        let mut txrefs = Vec::new();
        let mut unknown = HashSet::new();
        for (index, transaction) in transactions.iter().enumerate() {
            txrefs.push(UnminedTxId::from(transaction.as_ref().clone()).into());
            for copy in 0..5u8 {
                let mut hash = [0xCD; 32];
                // The index of a truncated list of 60 fits in a `u8`.
                hash[0] = index as u8;
                hash[1] = copy;
                let txref = TxRef::Txid(transaction::Hash(hash));
                unknown.insert(txref);
                txrefs.push(txref);
            }
        }
        assert!(txrefs.len() > 256, "more than one lookup chunk");
        let short_id = TxRef::ShortId {
            block: blocks[1].hash(),
            short_id: ShortTxId([1; 6]),
        };
        txrefs.insert(5, short_id);
        // A repeat of an earlier reference, from a later chunk.
        txrefs.push(txrefs[0]);

        let request = WireRequest::Tx(GetTx {
            txrefs: txrefs.clone(),
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Transaction>(&mut recv, txrefs.len())
            .await
            .expect("a well-formed response");
        assert_eq!(entries.len(), txrefs.len(), "every reference is answered");
        for (entry, txref) in entries.iter().zip(&txrefs) {
            match entry {
                ObjectResult::Found(transaction) => assert_eq!(
                    Some(UnminedTxId::from(transaction.as_ref().clone())),
                    txref.unmined_id()
                ),
                ObjectResult::NotFound => {
                    assert!(unknown.contains(txref) || *txref == short_id)
                }
            }
        }

        // Many copies of one transaction: answered from the first lookup, until the budget.
        let transaction = blocks
            .iter()
            .flat_map(|block| block.transactions.iter())
            .max_by_key(|transaction| entry_len(transaction))
            .expect("vector transactions")
            .clone();
        let len = entry_len(&transaction);
        let copies = 2 * RESPONSE_BYTES_LIMIT / len + 10;
        let expected = budget_prefix(std::iter::repeat_n(len, copies));
        assert!(expected < copies, "the budget cuts the response short");
        let request = WireRequest::Tx(GetTx {
            txrefs: vec![UnminedTxId::from(transaction.as_ref().clone()).into(); copies],
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Transaction>(&mut recv, copies)
            .await
            .expect("a well-formed response that finishes early");
        assert_eq!(entries.len(), expected);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `get-headers` request whose locator matches nothing gets headers from
/// height 1, not the genesis header; one whose locator ends in the genesis
/// hash is unchanged.
#[tokio::test]
async fn get_headers_without_a_known_locator_starts_at_height_1() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let raw = raw_peer_node(&network).await;
        let fixture = Fixture::new();
        let genesis = fixture.blocks[0].hash();
        let expected = vec![fixture.blocks[1].hash(), fixture.blocks[2].hash()];

        for locator in [vec![block::Hash([0xAB; 32])], vec![genesis]] {
            let request = WireRequest::Headers(GetHeaders {
                locator_hashes: locator.clone(),
                hash_stop: block::Hash([0; 32]),
                tx_ids: false,
            });
            let mut recv = raw_request(&raw.connection, &request).await;
            let headers = read_headers(&mut recv)
                .await
                .expect("a well-formed response");
            let hashes: Vec<_> = headers.iter().map(|header| header.hash()).collect();
            assert_eq!(hashes, expected, "locator {locator:?}");
            assert_eq!(headers[0].previous_block_hash, genesis);
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `get-mempool` snapshot holds at most the legacy limit of transaction
/// IDs, then the stream finishes.
#[tokio::test]
async fn mempool_snapshot_is_capped() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let max_ids: usize = MAX_TX_INV_IN_SENT_MESSAGE.try_into().expect("fits");
        let service = service_fn(move |request: Request| async move {
            Ok::<Response, BoxError>(match request {
                Request::MempoolTransactionIds => Response::TransactionIds(
                    (0..max_ids + 10)
                        .map(|index| {
                            let mut hash = [0; 32];
                            // The index fits in the hash's first 8 bytes.
                            hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
                            UnminedTxId::Legacy(transaction::Hash(hash))
                        })
                        .collect(),
                ),
                _ => Response::Nil,
            })
        });
        let raw = raw_peer_node_with(&network, Some(BoxCloneService::new(service))).await;

        let recv = raw_request(&raw.connection, &WireRequest::Mempool).await;
        let mut records = tokio_util::codec::FramedRead::new(recv, RecordCodec);
        let mut count = 0;
        while let Some(record) = records.next().await {
            record.expect("well-formed record");
            count += 1;
        }
        assert_eq!(count, max_ids);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `get-mempool` request from a raw peer gets the snapshot as records and
/// then a finished stream; a request on a known but unserved type is
/// refused with `REFUSED`; and the local client's own mempool request is
/// answered without a stream.
#[tokio::test]
async fn mempool_snapshot_and_unserved_requests() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = raw_peer_node(&network).await;
        let fixture = Fixture::new();

        // Requests this node cannot express on v2 are answered without a stream.
        let response = call(&mut raw.node.client, Request::MempoolTransactionIds)
            .await
            .expect("answered");
        assert_eq!(response, Response::TransactionIds(Vec::new()));

        // get-mempool: one record per transaction ID in the snapshot, then FIN.
        let (mut send, recv) = raw.connection.open_bi().await.expect("opens");
        send.write_all(&[StreamType::GetMempool.code()])
            .await
            .expect("writes");
        send.finish().expect("finishes");
        let mut records = tokio_util::codec::FramedRead::new(recv, RecordCodec);
        let mut ids = Vec::new();
        while let Some(record) = records.next().await {
            let payload = record.expect("well-formed record");
            let txref = crate::protocol::v2::announcement::decode_tx_record(&payload)
                .expect("a transaction reference");
            ids.push(txref.unmined_id().expect("txid or wtxid"));
        }
        let expected: Vec<UnminedTxId> = fixture.blocks[1..]
            .iter()
            .map(|b| UnminedTxId::from(b.transactions[0].as_ref().clone()))
            .collect();
        assert_eq!(ids, expected);

        // get-hashes is a known request type this node does not serve: REFUSED, connection open.
        let (mut send, mut recv) = raw.connection.open_bi().await.expect("opens");
        send.write_all(&[StreamType::GetHashes.code()])
            .await
            .expect("writes");
        send.finish().expect("finishes");
        let error = recv.read_to_end(16).await.expect_err("refused");
        assert!(
            matches!(error, quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))
                if code.into_inner() == ErrorCode::Refused.code()),
            "got {error:?}"
        );
        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Accepts the node's next request stream on the raw side and reads the
/// request to its end, returning the stream type byte and the stream.
async fn accept_request(raw: &Connection) -> (u8, SendStream, RecvStream) {
    let (send, mut recv) = raw
        .accept_bi()
        .await
        .expect("the node opens a request stream");
    let request = recv.read_to_end(1 << 20).await.expect("a complete request");
    (request[0], send, recv)
}

/// The response entry for a found `object`.
fn found<T: ZcashSerialize>(object: &Arc<T>) -> Vec<u8> {
    let mut bytes = Vec::new();
    ObjectResult::Found(object.clone())
        .write(&mut bytes)
        .expect("writes to a Vec");
    bytes
}

/// The response entry for an object the responder does not have.
fn not_found() -> Vec<u8> {
    let mut bytes = Vec::new();
    ObjectResult::<Arc<Block>>::NotFound
        .write(&mut bytes)
        .expect("writes to a Vec");
    bytes
}

/// How the raw peer answers one of the node's requests.
enum Reply {
    /// Writes these bytes, then finishes the stream.
    Bytes(Vec<u8>),
    /// Resets the stream with this code.
    Reset(ErrorCode),
}

/// The node checks each response to its requests: objects that are not the
/// ones asked for close the connection with `PROTOCOL_ERROR`; a non-contiguous
/// header chain is misbehavior; an explicit all-not-found response is the
/// legacy `NotFoundResponse` and registers the objects as missing; and a
/// response finished before its first entry, or refused, fails without
/// claiming anything is missing.
#[tokio::test]
async fn outbound_responses_are_verified() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let genesis = fixture.blocks[0].clone();
        let block_1 = fixture.blocks[1].clone();
        let block_2 = fixture.blocks[2].clone();
        let coinbase_1 = UnminedTxId::from(block_1.transactions[0].as_ref().clone());
        let missing = block::Hash([0xAB; 32]);

        for case in [
            "wrong block",
            "wrong transaction",
            "all not found",
            "finished before the first block",
            "finished before the first transaction",
            "non-contiguous headers",
            "refused",
        ] {
            let mut raw = raw_peer_node(&network).await;

            // The request, the raw peer's reply, what the client's error names, and the code the
            // connection closes with, if it does.
            let (request, reply, expected_error, close_code) = match case {
                "wrong block" => (
                    Request::BlockDownload {
                        hash: block_1.hash(),
                        advertiser: None,
                    },
                    Reply::Bytes(found(&block_2)),
                    "ProtocolViolation",
                    Some(ErrorCode::ProtocolError),
                ),
                "wrong transaction" => (
                    Request::TransactionsById([coinbase_1].into_iter().collect()),
                    Reply::Bytes(found(&block_2.transactions[0])),
                    "ProtocolViolation",
                    Some(ErrorCode::ProtocolError),
                ),
                "all not found" => (
                    Request::BlocksByHash([block_1.hash(), missing].into_iter().collect()),
                    Reply::Bytes([not_found(), not_found()].concat()),
                    "NotFoundResponse",
                    None,
                ),
                "finished before the first block" => (
                    Request::BlockDownload {
                        hash: block_1.hash(),
                        advertiser: None,
                    },
                    Reply::Bytes(Vec::new()),
                    "RequestStreamFailed",
                    None,
                ),
                "finished before the first transaction" => (
                    Request::TransactionsById([coinbase_1].into_iter().collect()),
                    Reply::Bytes(Vec::new()),
                    "RequestStreamFailed",
                    None,
                ),
                "non-contiguous headers" => {
                    let mut bytes = Vec::new();
                    write_headers_response(
                        &mut bytes,
                        &[block_2.header.clone(), block_1.header.clone()],
                        false,
                    )
                    .expect("writes to a Vec");
                    (
                        Request::FindHeaders {
                            known_blocks: vec![genesis.hash()],
                            stop: None,
                        },
                        Reply::Bytes(bytes),
                        "not a contiguous chain",
                        None,
                    )
                }
                "refused" => (
                    Request::BlockDownload {
                        hash: block_1.hash(),
                        advertiser: None,
                    },
                    Reply::Reset(ErrorCode::Refused),
                    "RequestStreamFailed",
                    None,
                ),
                _ => unreachable!(),
            };

            let (result, ()) = tokio::join!(call(&mut raw.node.client, request), async {
                let (_, mut send, _recv) = accept_request(&raw.connection).await;
                // The node may close the connection as soon as it reads a bad entry.
                match reply {
                    Reply::Bytes(bytes) => {
                        let _ = send.write_all(&bytes).await;
                        let _ = send.finish();
                    }
                    Reply::Reset(code) => {
                        let _ = send.reset(app_error_code(code));
                    }
                }
            });
            let error = result.expect_err(case);
            let inner = error.inner_debug();
            assert!(
                inner.contains(expected_error),
                "{case}: expected {expected_error}, got {inner}"
            );

            // Only an explicit not-found registers missing inventory.
            match case {
                "all not found" => {
                    let change = raw.node.inv_rx.try_recv().expect("missing inventory");
                    let InventoryChange::Missing((hashes, _)) = change else {
                        panic!("{case}: unexpected change {change:?}");
                    };
                    let hashes: HashSet<_> = hashes.iter().copied().collect();
                    let expected: HashSet<_> = [block_1.hash(), missing]
                        .into_iter()
                        .map(InventoryHash::Block)
                        .collect();
                    assert_eq!(hashes, expected, "{case}");
                }
                _ => assert!(
                    raw.node.inv_rx.try_recv().is_err(),
                    "{case}: nothing is registered as missing"
                ),
            }
            assert!(
                !inner.contains("NotFoundResponse") || case == "all not found",
                "{case}: got {inner}"
            );

            match close_code {
                Some(code) => {
                    let close = raw.connection.closed().await;
                    assert!(
                        matches!(&close, quinn::ConnectionError::ApplicationClosed(close)
                            if close.error_code.into_inner() == code.code()),
                        "{case}: got {close:?}"
                    );
                }
                None => {
                    call(&mut raw.node.client, Request::Ping(Default::default()))
                        .await
                        .unwrap_or_else(|error| panic!("{case}: the client fails: {error}"));
                    assert!(
                        raw.connection.close_reason().is_none(),
                        "{case}: the connection stays open"
                    );
                }
            }
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `BlocksByHash` request larger than one stream allows keeps the answers
/// of every stream: one found block among made-up hashes comes back with
/// the rest missing, whichever stream it lands in, and only the missing
/// hashes are registered as missing. An all-missing request is the legacy
/// `NotFoundResponse`.
#[tokio::test]
async fn chunked_inventory_requests_keep_every_chunk() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (mut a, _b, _e1, _e2) = connected_nodes(&network).await;
        let block = Fixture::new().blocks[1].clone();
        let made_up = |count: u8| -> HashSet<block::Hash> {
            (0..count)
                .map(|index| {
                    let mut hash = [0xEE; 32];
                    hash[0] = index;
                    block::Hash(hash)
                })
                .collect()
        };

        let mut hashes = made_up(199);
        hashes.insert(block.hash());
        assert!(hashes.len() > crate::protocol::v2::constants::MAX_BLOCKS_PER_REQUEST);
        let response = call(&mut a.client, Request::BlocksByHash(hashes.clone()))
            .await
            .expect("a partial response");
        let Response::Blocks(blocks) = response else {
            panic!("unexpected response {response}");
        };
        assert_eq!(blocks.len(), 200);
        let available: Vec<_> = blocks.iter().filter_map(|b| b.available()).collect();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].0.hash(), block.hash());
        let missing: HashSet<_> = blocks.iter().filter_map(|b| b.missing()).collect();
        assert_eq!(missing, made_up(199));

        let change = a.inv_rx.try_recv().expect("missing inventory");
        let InventoryChange::Missing((registered, _)) = change else {
            panic!("unexpected change {change:?}");
        };
        let registered: HashSet<_> = registered.iter().copied().collect();
        let expected: HashSet<_> = made_up(199).into_iter().map(InventoryHash::Block).collect();
        assert_eq!(registered, expected);

        let error = call(&mut a.client, Request::BlocksByHash(made_up(200)))
            .await
            .expect_err("nothing found");
        assert!(
            error.inner_debug().contains("NotFoundResponse"),
            "got {}",
            error.inner_debug()
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// While `MAX_OUTBOUND_REQUEST_STREAMS` requests are in flight, the client
/// takes one more and is then unready, like a busy legacy peer; when a
/// response arrives, the queued request is sent and the client is ready.
#[tokio::test]
async fn outbound_request_streams_are_bounded() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = raw_peer_node(&network).await;
        let hash = Fixture::new().blocks[1].hash();
        let request = || Request::BlockDownload {
            hash,
            advertiser: None,
        };

        // Requests the raw peer accepts and never answers.
        let mut responses = Vec::new();
        let mut streams = Vec::new();
        for _ in 0..MAX_OUTBOUND_REQUEST_STREAMS {
            raw.node.client.ready().await.expect("ready");
            responses.push(raw.node.client.call(request()));
            streams.push(accept_request(&raw.connection).await);
        }

        // One more is taken and queued, without a stream.
        raw.node.client.ready().await.expect("ready");
        responses.push(raw.node.client.call(request()));
        assert!(
            tokio::time::timeout(SHORT_WAIT, raw.node.client.ready())
                .await
                .is_err(),
            "the client is unready while every outbound stream is in use"
        );
        assert!(
            tokio::time::timeout(SHORT_WAIT, raw.connection.accept_bi())
                .await
                .is_err(),
            "the queued request has no stream yet"
        );

        // One response frees a stream for the queued request.
        let (_, mut send, _recv) = streams.remove(0);
        send.write_all(&not_found()).await.expect("writes");
        send.finish().expect("finishes");
        let error = responses.remove(0).await.expect_err("not found");
        assert!(error.inner_debug().contains("NotFoundResponse"));
        streams.push(accept_request(&raw.connection).await);
        raw.node.client.ready().await.expect("ready again");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A request whose client stops waiting is abandoned at once: the node
/// cancels the stream with `CANCELLED`, and the connection stays usable.
#[tokio::test]
async fn cancelled_request_stops_its_stream() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = raw_peer_node(&network).await;
        let block = Fixture::new().blocks[1].clone();
        let request = Request::BlockDownload {
            hash: block.hash(),
            advertiser: None,
        };

        raw.node.client.ready().await.expect("ready");
        let response = raw.node.client.call(request.clone());
        let (_, send, _recv) = accept_request(&raw.connection).await;
        drop(response);
        let stopped = tokio::time::timeout(REQUEST_TIMEOUT / 4, send.stopped())
            .await
            .expect("stopped well within the request timeout")
            .expect("the connection is open");
        assert_eq!(stopped, Some(app_error_code(ErrorCode::Cancelled)));

        let (result, ()) = tokio::join!(call(&mut raw.node.client, request), async {
            let (_, mut send, _recv) = accept_request(&raw.connection).await;
            send.write_all(&found(&block)).await.expect("writes");
            send.finish().expect("finishes");
        });
        assert!(matches!(result, Ok(Response::Blocks(blocks)) if blocks.len() == 1));
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Requests end at `REQUEST_TIMEOUT` and the connection stays open: a
/// request to a peer that never answers fails with
/// `ConnectionReceiveTimeout` and cancels its stream with `CANCELLED`, and
/// a request from the peer that waits for one of the node's busy request
/// permits is refused with `REFUSED`.
///
/// This takes `REQUEST_TIMEOUT` of real time: pausing tokio's clock is
/// unsafe with real sockets.
#[tokio::test]
async fn stalled_requests_end_at_the_request_timeout() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let hanging = service_fn(|_request: Request| futures::future::pending());
        let mut raw = raw_peer_node_with(&network, Some(BoxCloneService::new(hanging))).await;
        let hash = Fixture::new().blocks[1].hash();
        let started = Instant::now();

        let outbound = async {
            let (result, send) = tokio::join!(
                call(
                    &mut raw.node.client,
                    Request::BlockDownload {
                        hash,
                        advertiser: None,
                    },
                ),
                async {
                    let (_, send, recv) = accept_request(&raw.connection).await;
                    (send, recv)
                }
            );
            let error = result.expect_err("the request times out");
            assert!(
                error.inner_debug().contains("ConnectionReceiveTimeout"),
                "got {}",
                error.inner_debug()
            );
            assert!(started.elapsed() >= REQUEST_TIMEOUT - Duration::from_secs(1));
            let stopped = tokio::time::timeout(SHORT_WAIT * 10, send.0.stopped())
                .await
                .expect("stopped when the request times out")
                .expect("the connection is open");
            assert_eq!(stopped, Some(app_error_code(ErrorCode::Cancelled)));
        };

        let inbound = async {
            let request = WireRequest::Blocks(GetBlocks { hashes: vec![hash] });
            let mut recvs = Vec::new();
            for _ in 0..=INBOUND_REQUEST_CONCURRENCY {
                recvs.push(raw_request(&raw.connection, &request).await);
            }
            let reads = recvs
                .iter_mut()
                .map(|recv| Box::pin(recv.read_to_end(1024)))
                .collect::<Vec<_>>();
            // The requests holding the permits wait on the inbound service; the extra one is
            // refused at its deadline.
            let (first, _index, rest) = futures::future::select_all(reads).await;
            assert_reset(first, ErrorCode::Refused);
            assert!(started.elapsed() >= REQUEST_TIMEOUT - Duration::from_secs(1));
            assert!(
                tokio::time::timeout(SHORT_WAIT, futures::future::select_all(rest))
                    .await
                    .is_err(),
                "the requests holding permits are still waiting"
            );
        };

        tokio::join!(outbound, inbound);
        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
        call(&mut raw.node.client, Request::Ping(Default::default()))
            .await
            .expect("the client is usable");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// `get-addr` is only sent on a connection this node dialled, and only
/// once: later `Request::Peers` are answered from the cache, even when it
/// is empty, as they are on a connection the peer dialled.
#[tokio::test]
async fn get_addr_is_only_sent_once_on_dialled_connections() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (mut a, mut b, _e1, _e2) = connected_nodes(&network).await;
        let fixture = Fixture::new();

        // B accepted the connection: its empty cache answers.
        let response = call(&mut b.client, Request::Peers).await.expect("peers");
        assert_eq!(response, Response::Peers(Vec::new()));

        // A dialled it: the first request asks B, the second is answered from the cache.
        let response = call(&mut a.client, Request::Peers).await.expect("peers");
        assert!(
            matches!(&response, Response::Peers(addrs) if addrs.len() == 1
                && addrs[0].addr() == fixture.addrs[0].addr()),
            "got {response}"
        );
        assert!(matches!(b.inbound_rx.recv().await, Some(Request::Peers)));
        let response = call(&mut a.client, Request::Peers).await.expect("peers");
        assert_eq!(response, Response::Peers(Vec::new()));

        assert!(b.inbound_rx.try_recv().is_err(), "one get-addr reached B");
        assert!(a.inbound_rx.try_recv().is_err(), "no get-addr reached A");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `get-addr` request is refused with `REFUSED` on a connection the node
/// dialled, and after the first one on a connection the peer dialled; the
/// connection stays open.
#[tokio::test]
async fn get_addr_is_refused_unless_first_and_inbound() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (_a, _b, (a_connection, b_connection), _e1, _e2) =
            connected_nodes_with_connections(&network).await;

        // From B, which accepted the connection, to A, which dialled it.
        let mut recv = raw_request(&b_connection, &WireRequest::Addr).await;
        assert_reset(recv.read_to_end(1 << 20).await, ErrorCode::Refused);

        // From A to B: answered once.
        let mut recv = raw_request(&a_connection, &WireRequest::Addr).await;
        let body = recv.read_to_end(1 << 20).await.expect("answered");
        let addrs = decode_addr_response(&body).expect("a well-formed response");
        assert_eq!(addrs.len(), 1);
        let mut recv = raw_request(&a_connection, &WireRequest::Addr).await;
        assert_reset(recv.read_to_end(1 << 20).await, ErrorCode::Refused);

        assert!(a_connection.close_reason().is_none());
        assert!(b_connection.close_reason().is_none());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// The peer's address announcements fill the cache that answers
/// `Request::Peers`, without the inbound service; its block announcements
/// reach the inventory registry and the inbound service.
#[tokio::test]
async fn inbound_announcements_reach_the_node() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = raw_peer_node(&network).await;

        let addrs: Vec<MetaAddr> = ["203.0.113.7:8233", "203.0.113.8:8233"]
            .into_iter()
            .map(|addr| {
                MetaAddr::new_gossiped_meta_addr(
                    addr.parse::<SocketAddr>().unwrap().into(),
                    PeerServices::NODE_NETWORK,
                    DateTime32::now(),
                )
            })
            .collect();
        let mut send = raw.connection.open_uni().await.expect("opens");
        send.write_all(&[StreamType::AddressAnnouncements.code()])
            .await
            .expect("writes");
        for addr in &addrs {
            // A one-address `get-addr` response is a one-byte count, then the record.
            let response = encode_addr_response(std::slice::from_ref(addr)).expect("encodes");
            let record = RecordCodec::encode_record(&response[1..]).expect("valid");
            send.write_all(&record).await.expect("writes");
        }

        // The raw peer dialled, so `Request::Peers` is answered from the cache, which the
        // announcements fill once their reader has handed them over.
        let mut received = Vec::new();
        while received.len() < addrs.len() {
            match call(&mut raw.node.client, Request::Peers).await {
                Ok(Response::Peers(peers)) => received.extend(peers),
                other => panic!("unexpected response {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let received: HashSet<_> = received.iter().map(|addr| addr.addr()).collect();
        let expected: HashSet<_> = addrs.iter().map(|addr| addr.addr()).collect();
        assert_eq!(received, expected);
        assert!(
            raw.node.inbound_rx.try_recv().is_err(),
            "the inbound service was not asked"
        );

        let block = Fixture::new().blocks[1].clone();
        let mut send = raw.connection.open_uni().await.expect("opens");
        send.write_all(&[StreamType::BlockAnnouncements.code()])
            .await
            .expect("writes");
        let mut payload = vec![BLOCK_ANNOUNCEMENT_HEADER];
        block
            .header
            .zcash_serialize(&mut payload)
            .expect("writes to a Vec");
        let record = RecordCodec::encode_record(&payload).expect("valid");
        send.write_all(&record).await.expect("writes");

        let change = raw.node.inv_rx.recv().await.expect("an inventory change");
        assert!(
            matches!(&change, InventoryChange::Available((hashes, _))
                if hashes.first() == &InventoryHash::Block(block.hash())),
            "got {change:?}"
        );
        let request = raw.node.inbound_rx.recv().await.expect("a request");
        assert!(
            matches!(request, Request::AdvertiseBlock(hash, Some(_)) if hash == block.hash()),
            "got {request}"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// The node opens no transaction announcement stream to a peer whose `init`
/// record declined relay.
#[tokio::test]
async fn no_transaction_stream_to_a_non_relay_peer() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = raw_peer_node_relaying(&network, None, false).await;
        let fixture = Fixture::new();
        let id = UnminedTxId::from(fixture.blocks[1].transactions[0].as_ref().clone());
        let tx = UnminedTx::from(fixture.blocks[2].transactions[0].clone());

        let response = call(
            &mut raw.node.client,
            Request::AdvertiseTransactionIds([id].into_iter().collect(), None),
        )
        .await
        .expect("advertise");
        assert_eq!(response, Response::Nil);
        call(&mut raw.node.client, Request::PushTransaction(tx, None))
            .await
            .expect("push");

        assert!(
            tokio::time::timeout(SHORT_WAIT, raw.connection.accept_uni())
                .await
                .is_err(),
            "no announcement stream is opened"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An announcement writer whose peer stops reading gives up after its write
/// timeout and resets the stream with `CANCELLED`, rather than finishing it
/// part way through a record.
#[tokio::test]
async fn stalled_announcement_writer_resets_its_stream() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (writer_connection, reader_connection, _e1, _e2) = connected_pair(&network).await;
        let mut tasks = JoinSet::new();
        let write_timeout = Duration::from_millis(200);
        let writer = AnnouncementWriter::spawn(
            writer_connection.clone(),
            StreamType::TransactionAnnouncements,
            &mut tasks,
            write_timeout,
        );

        // More records than the reader's 4 MiB stream receive window, so the write stalls.
        let id = UnminedTxId::from(Fixture::new().blocks[1].transactions[0].as_ref().clone());
        let record = RecordCodec::encode_record(encode_tx_record(id)).expect("valid");
        let batch = record.repeat(5 * 1024 * 1024 / record.len() + 1);
        assert!(writer.announce(batch.into()).is_ok());

        let mut recv = reader_connection.accept_uni().await.expect("accepts");
        // Nothing is read until the writer has given up.
        tasks
            .join_next()
            .await
            .expect("the writer task")
            .expect("the writer task does not panic");
        let read = recv.read_to_end(usize::MAX).await;
        assert_reset(read, ErrorCode::Cancelled);
        assert!(
            writer.announce(record).is_err(),
            "the writer reports its stream ended"
        );
        assert!(reader_connection.close_reason().is_none());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A request abandoned before it is completely written resets the stream
/// with `CANCELLED`, rather than finishing it, which the responder would
/// read as a truncated request.
#[tokio::test]
async fn abandoned_request_write_resets_the_stream() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let (mut send, _recv) = requester.open_bi().await.expect("opens");
        send.write_all(&[StreamType::GetBlocks.code()])
            .await
            .expect("writes");
        drop(RequestSend(Some(send)));

        let (_send, mut recv) = responder.accept_bi().await.expect("accepts");
        assert_reset(recv.read_to_end(1024).await, ErrorCode::Cancelled);
    })
    .await
    .expect("the test completes well within the timeout");
}
