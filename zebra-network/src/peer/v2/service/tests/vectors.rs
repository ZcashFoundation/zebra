//! End-to-end tests: two full v2 peer stacks over loopback QUIC.

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use indexmap::IndexMap;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{Block, CountedHeader},
    chain_tip::NoChainTip,
    parameters::Network,
    serialization::{DateTime32, ZcashDeserializeInto},
    transaction::UnminedTx,
};

use crate::{
    meta_addr::MetaAddr,
    peer::{
        v2::service::{Handshake as V2Handshake, HandshakeRequest as V2HandshakeRequest},
        Client, ConnectedAddr, MinimumPeerVersion,
    },
    peer_set::ActiveConnectionCounter,
    protocol::{
        external::types::PeerServices,
        internal::{InventoryResponse, Request, Response},
        v2::{
            constants::{
                MAX_CONSECUTIVE_REQUEST_TIMEOUTS, MAX_HEADERS_RESULTS,
                MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED,
            },
            quic,
        },
    },
    BoxError, Config, PeerSocketAddr,
};

/// A generous timeout for loopback test operations.
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The test data served by a [`TestInbound`] service.
struct TestData {
    /// Contiguous mainnet headers (blocks 1 and 2).
    headers: Vec<CountedHeader>,

    /// Mainnet block 1: the block of `headers[0]`.
    block_1: Arc<Block>,

    /// Mainnet block 2: the block of `headers[1]`, and the child of
    /// block 1.
    block_2: Arc<Block>,

    /// A block with several transactions.
    block: Arc<Block>,

    /// The blocks the inbound service serves by hash.
    ///
    /// Tests that need an unavailable block remove it from this map.
    served_blocks: IndexMap<zebra_chain::block::Hash, Arc<Block>>,

    /// A mempool transaction served by ID.
    transaction: UnminedTx,

    /// The peers returned for address requests.
    peers: Vec<MetaAddr>,

    /// If set, the inbound service never answers, simulating a peer whose
    /// QUIC stack is alive but whose application is wedged.
    hang_inbound: bool,
}

impl TestData {
    fn new() -> Self {
        let block_1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block test vector parses")
            .into();
        let block_2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block test vector parses")
            .into();

        let transaction = UnminedTx::from(block_2.transactions[0].clone());

        // A block with several transactions, so compact blocks built from it
        // have short transaction IDs.
        let multi_tx_block: Arc<Block> = zebra_test::vectors::MAINNET_BLOCKS
            .values()
            .map(|bytes| {
                bytes
                    .zcash_deserialize_into::<Block>()
                    .expect("block test vectors parse")
            })
            .find(|block| block.transactions.len() >= 3)
            .expect("some mainnet block test vector has at least 3 transactions")
            .into();

        TestData {
            headers: vec![
                CountedHeader {
                    header: block_1.header.clone(),
                },
                CountedHeader {
                    header: block_2.header.clone(),
                },
            ],
            served_blocks: [&block_1, &block_2, &multi_tx_block]
                .into_iter()
                .map(|block| (block.hash(), block.clone()))
                .collect(),
            block_1: block_1.clone(),
            block_2: block_2.clone(),
            block: multi_tx_block,
            transaction,
            peers: vec![MetaAddr::new_gossiped_meta_addr(
                "203.0.113.7:8233".parse().expect("valid address"),
                PeerServices::NODE_NETWORK,
                DateTime32::from(1_700_000_000),
            )],
            hang_inbound: false,
        }
    }
}

/// The fixed `get-hashes` entries served by [`TestInbound`].
fn test_sync_hash_entries(data: &TestData) -> Vec<zebra_chain::block::SyncHashEntry> {
    vec![
        zebra_chain::block::SyncHashEntry {
            hash: data.block_1.hash(),
            span_size: 1,
            span_txs: 1,
            span_notes: 0,
        },
        zebra_chain::block::SyncHashEntry {
            hash: data.block_2.hash(),
            span_size: 27,
            span_txs: 420,
            span_notes: 77,
        },
    ]
}

/// The fixed `get-tree-roots` entries served by [`TestInbound`].
fn test_tree_roots_entries() -> Vec<zebra_chain::block::TreeRootsEntry> {
    vec![zebra_chain::block::TreeRootsEntry {
        sapling_root: [0x0A; 32],
        orchard_root: [0; 32],
        ironwood_root: [0; 32],
        sapling_txs: 2,
        orchard_txs: 0,
        ironwood_txs: 0,
        auth_data_root: [0xAD; 32],
    }]
}

/// A minimal inbound service for tests, serving fixed test data and
/// capturing gossip requests.
#[derive(Clone)]
struct TestInbound {
    data: Arc<TestData>,
    events: tokio::sync::mpsc::UnboundedSender<Request>,
}

impl Service<Request> for TestInbound {
    type Response = Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let data = self.data.clone();
        let events = self.events.clone();

        if data.hang_inbound {
            return Box::pin(futures::future::pending());
        }

        Box::pin(async move {
            let response = match request {
                Request::FindHeaders { .. } => Response::BlockHeaders(data.headers.clone()),
                Request::BlocksByHash(hashes) => Response::Blocks(
                    hashes
                        .into_iter()
                        .map(|hash| match data.served_blocks.get(&hash) {
                            Some(block) => InventoryResponse::Available((block.clone(), None)),
                            None => InventoryResponse::Missing(hash),
                        })
                        .collect(),
                ),
                Request::TransactionsById(ids) => Response::Transactions(
                    ids.into_iter()
                        .map(|id| {
                            if id == data.transaction.id {
                                InventoryResponse::Available((data.transaction.clone(), None))
                            } else {
                                InventoryResponse::Missing(id)
                            }
                        })
                        .collect(),
                ),
                Request::MempoolTransactionIds => {
                    Response::TransactionIds(vec![data.transaction.id])
                }
                Request::SyncHashes { .. } => Response::SyncHashes(test_sync_hash_entries(&data)),
                Request::TreeRoots { final_hash, .. } => Response::TreeRoots(
                    (final_hash == data.block.hash()).then(test_tree_roots_entries),
                ),
                Request::Peers => Response::Peers(data.peers.clone()),
                request @ (Request::AdvertiseTransactionIds(_, _)
                | Request::AdvertiseBlock(_, _)
                | Request::PushTransaction(_, _)) => {
                    let _ = events.send(request);
                    Response::Nil
                }
                other => unreachable!("unexpected inbound request in test: {other:?}"),
            };

            Ok(response)
        })
    }
}

/// Builds a version 2 handshake service serving `data`, and the channel
/// that captures the gossip requests its inbound service receives.
fn test_handshaker(
    network: &Network,
    data: Arc<TestData>,
    misbehavior_tx: tokio::sync::mpsc::Sender<(PeerSocketAddr, u32)>,
) -> (
    V2Handshake<TestInbound, NoChainTip>,
    tokio::sync::mpsc::UnboundedReceiver<Request>,
) {
    let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();

    let handshaker = V2Handshake::new(
        Config {
            network: network.clone(),
            ..Config::default()
        },
        "/ZebraV2Test:0.0.1/".to_string(),
        PeerServices::NODE_NETWORK,
        true,
        TestInbound {
            data,
            events: events_tx,
        },
        crate::peer_book::ChangeSender::new(tokio::sync::mpsc::channel(100).0),
        misbehavior_tx,
        tokio::sync::broadcast::channel(100).0,
        MinimumPeerVersion::new(NoChainTip, network),
        "127.0.0.1:8233".parse().expect("valid address"),
    );

    (handshaker, events)
}

/// Opens a loopback QUIC endpoint, and returns it with its bound address.
///
/// The endpoint is leaked, because dropping it closes every connection it
/// opened.
fn test_endpoint(network: &Network) -> (quinn::Endpoint, std::net::SocketAddr) {
    let endpoint = quic::new_endpoint("127.0.0.1:0".parse().expect("valid address"), network)
        .expect("endpoint creation succeeds");
    let addr = endpoint.local_addr().expect("endpoint has a local address");

    (endpoint, addr)
}

/// A connected pair of v2 peers: each side's [`Client`] and captured inbound
/// gossip events.
///
/// Tests must keep the whole struct alive: dropping either [`Client`]
/// aborts that side's connection task.
struct ConnectedPeers {
    initiator_client: Client,
    responder_client: Client,
    /// Held for symmetry with `responder_events`; no test asserts on
    /// initiator-side gossip yet.
    #[allow(dead_code)]
    initiator_events: tokio::sync::mpsc::UnboundedReceiver<Request>,
    responder_events: tokio::sync::mpsc::UnboundedReceiver<Request>,
    /// The misbehavior scores the initiator assigned to the responder.
    initiator_misbehavior: tokio::sync::mpsc::Receiver<(PeerSocketAddr, u32)>,
}

/// Builds two full v2 peer stacks over loopback QUIC and connects them.
async fn connect_peers(data: Arc<TestData>) -> ConnectedPeers {
    let network = Network::Mainnet;

    let initiator_endpoint =
        quic::new_endpoint("127.0.0.1:0".parse().expect("valid address"), &network)
            .expect("endpoint creation succeeds");
    let (responder_endpoint, responder_addr) = test_endpoint(&network);

    let (initiator_misbehavior_tx, initiator_misbehavior) = tokio::sync::mpsc::channel(100);

    let (mut initiator_handshaker, initiator_events) =
        test_handshaker(&network, data.clone(), initiator_misbehavior_tx);
    let (mut responder_handshaker, responder_events) =
        test_handshaker(&network, data, tokio::sync::mpsc::channel(100).0);

    let mut counter = ActiveConnectionCounter::new_counter();

    let responder_task = {
        let responder_tracker = counter.track_connection();
        tokio::spawn(async move {
            let incoming = responder_endpoint.accept().await.expect("endpoint accepts");
            let connection = quic::accept(incoming, &Network::Mainnet)
                .await
                .expect("inbound connection completes");
            let connected_addr =
                ConnectedAddr::new_inbound_direct(connection.remote_address().into());

            let client = responder_handshaker
                .ready()
                .await
                .expect("handshaker is ready")
                .call(V2HandshakeRequest {
                    connection,
                    connected_addr,
                    connection_tracker: responder_tracker,
                })
                .await
                .expect("responder handshake succeeds");

            // Keep the endpoint alive with the connection.
            std::mem::forget(responder_endpoint);
            client
        })
    };

    let connection = quic::connect(&initiator_endpoint, responder_addr, &network)
        .await
        .expect("outbound connection completes");
    std::mem::forget(initiator_endpoint);

    let initiator_client = initiator_handshaker
        .ready()
        .await
        .expect("handshaker is ready")
        .call(V2HandshakeRequest {
            connection,
            connected_addr: ConnectedAddr::new_outbound_direct(responder_addr.into()),
            connection_tracker: counter.track_connection(),
        })
        .await
        .expect("initiator handshake succeeds");

    let responder_client = responder_task.await.expect("responder task succeeds");

    ConnectedPeers {
        initiator_client,
        responder_client,
        initiator_events,
        responder_events,
        initiator_misbehavior,
    }
}

/// Sends `request` through `client` with a timeout.
async fn call(client: &mut Client, request: Request) -> Result<Response, BoxError> {
    let response = tokio::time::timeout(
        TEST_TIMEOUT,
        client.ready().await.expect("client is ready").call(request),
    )
    .await
    .expect("response arrives in time")?;

    Ok(response)
}

/// A full v2 responder stack, driven by a raw wire-level initiator.
///
/// Tests must keep the struct alive: dropping the responder's [`Client`]
/// aborts its connection task, and dropping the raw handshake closes the
/// initiator's handshake stream.
struct RawInitiator {
    /// The test data the responder serves.
    data: Arc<TestData>,

    /// The initiator's QUIC connection, for opening raw streams.
    connection: quinn::Connection,

    /// The initiator's completed handshake, held to keep its streams open.
    #[allow(dead_code)]
    handshake: crate::peer::v2::handshake::V2Handshake,

    /// The responder's peer client, held to keep its connection task alive.
    #[allow(dead_code)]
    responder_client: Client,

    /// The gossip requests the responder's inbound service received.
    responder_events: tokio::sync::mpsc::UnboundedReceiver<Request>,

    /// The misbehavior scores the responder assigned to the initiator.
    responder_misbehavior: tokio::sync::mpsc::Receiver<(PeerSocketAddr, u32)>,
}

/// Builds a full v2 responder stack serving `data` over loopback QUIC, and
/// connects to it with a raw initiator that completes only the handshake,
/// leaving the test in control of every stream.
async fn raw_initiator(data: Arc<TestData>) -> RawInitiator {
    raw_initiator_impl(data, false, false, None).await
}

/// Like [`raw_initiator`], with the initiator requesting high-bandwidth
/// compact block announcements (`announce`) and full transaction IDs in
/// them (`full_ids`) in its `init` record.
async fn raw_initiator_with_init(
    data: Arc<TestData>,
    announce: bool,
    full_ids: bool,
) -> RawInitiator {
    raw_initiator_impl(data, announce, full_ids, None).await
}

/// Like [`raw_initiator`], with a custom responder configuration; its
/// network must be Mainnet.
async fn raw_initiator_with_config(data: Arc<TestData>, config: Config) -> RawInitiator {
    raw_initiator_impl(data, false, false, Some(config)).await
}

async fn raw_initiator_impl(
    data: Arc<TestData>,
    announce: bool,
    full_ids: bool,
    config: Option<Config>,
) -> RawInitiator {
    use crate::{
        peer::{
            handshake::HandshakeNonces,
            v2::handshake::{initiate, HandshakeParams},
        },
        protocol::{
            external::types::Nonce,
            v2::{constants::MIN_V2_PROTOCOL_VERSION, init::InitRecord},
        },
    };

    let network = Network::Mainnet;
    let (responder_endpoint, responder_addr) = test_endpoint(&network);

    let (responder_misbehavior_tx, responder_misbehavior) = tokio::sync::mpsc::channel(100);
    let (mut responder_handshaker, responder_events) = match config {
        // One test needs a custom cache directory, which only the full
        // constructor takes.
        Some(config) => {
            let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
            let handshaker = V2Handshake::new(
                config,
                "/ZebraV2Test:0.0.1/".to_string(),
                PeerServices::NODE_NETWORK,
                true,
                TestInbound {
                    data: data.clone(),
                    events: events_tx,
                },
                crate::peer_book::ChangeSender::new(tokio::sync::mpsc::channel(100).0),
                responder_misbehavior_tx,
                tokio::sync::broadcast::channel(100).0,
                MinimumPeerVersion::new(NoChainTip, &network),
                "127.0.0.1:8233".parse().expect("valid address"),
            );

            (handshaker, events)
        }
        None => test_handshaker(&network, data.clone(), responder_misbehavior_tx),
    };

    let mut counter = ActiveConnectionCounter::new_counter();
    let responder_tracker = counter.track_connection();
    let responder_task = tokio::spawn(async move {
        let incoming = responder_endpoint.accept().await.expect("endpoint accepts");
        let connection = quic::accept(incoming, &Network::Mainnet)
            .await
            .expect("inbound connection completes");
        let connected_addr = ConnectedAddr::new_inbound_direct(connection.remote_address().into());

        let client = responder_handshaker
            .ready()
            .await
            .expect("handshaker is ready")
            .call(V2HandshakeRequest {
                connection,
                connected_addr,
                connection_tracker: responder_tracker,
            })
            .await
            .expect("responder handshake succeeds");

        std::mem::forget(responder_endpoint);
        client
    });

    let (initiator_endpoint, _initiator_addr) = test_endpoint(&network);
    let connection = quic::connect(&initiator_endpoint, responder_addr, &network)
        .await
        .expect("outbound connection completes");
    std::mem::forget(initiator_endpoint);

    let params = HandshakeParams {
        local_init: InitRecord {
            version: MIN_V2_PROTOCOL_VERSION,
            services: PeerServices::NODE_NETWORK,
            nonce: Nonce::default(),
            user_agent: "/ZebraV2RawTest:0.0.1/".to_string(),
            start_height: zebra_chain::block::Height(0),
            relay: true,
            announce,
            full_ids,
        },
        min_remote_version: MIN_V2_PROTOCOL_VERSION,
        nonces: HandshakeNonces::default(),
        nonce_limit: 100,
    };
    let handshake = tokio::time::timeout(TEST_TIMEOUT, initiate(&connection, &params))
        .await
        .expect("handshake completes in time")
        .expect("initiator handshake succeeds");
    let responder_client = responder_task.await.expect("responder task succeeds");

    RawInitiator {
        data,
        connection,
        handshake,
        responder_client,
        responder_events,
        responder_misbehavior,
    }
}

/// Test that a peer whose QUIC stack is alive but whose application never
/// answers a request stream is disconnected, instead of sinking every request
/// routed to it for the process lifetime.
///
/// This test waits for real request timeouts, so it takes about
/// [`REQUEST_TIMEOUT`](crate::constants::REQUEST_TIMEOUT) to run: the
/// requests are made concurrently so they time out together.
#[tokio::test]
async fn v2_unresponsive_peer_is_disconnected() {
    let _init_guard = zebra_test::init();

    let mut data = TestData::new();
    data.hang_inbound = true;

    let mut peers = connect_peers(Arc::new(data)).await;

    // Enough concurrent requests to reach the consecutive timeout limit.
    // The requests must need the wire: mempool requests are answered from
    // the local subscription mirror.
    let mut requests = Vec::new();
    for _ in 0..MAX_CONSECUTIVE_REQUEST_TIMEOUTS {
        let mut client = peers
            .initiator_client
            .ready()
            .await
            .expect("client is ready")
            .call(Request::FindHeaders {
                known_blocks: vec![],
                stop: None,
            });
        requests.push(std::future::poll_fn(move |cx| {
            Pin::new(&mut client).poll(cx)
        }));
    }

    let results = tokio::time::timeout(
        crate::constants::REQUEST_TIMEOUT + TEST_TIMEOUT,
        futures::future::join_all(requests),
    )
    .await
    .expect("the requests time out in time");

    for result in results {
        assert!(
            result.is_err(),
            "an unanswered request must fail, got: {result:?}",
        );
    }

    // The connection was closed, so the peer leaves the peer set instead of
    // sinking further requests.
    let result = peers.initiator_client.ready().await;
    assert!(
        result.is_err(),
        "an unresponsive peer must be disconnected, got: {result:?}",
    );
}

#[tokio::test]
async fn v2_oversized_get_headers_response_is_scored() {
    let _init_guard = zebra_test::init();

    // A `get-headers` response over the limit is a scored violation, not a
    // connection error.
    let mut data = TestData::new();
    let header = data.headers[0].clone();
    data.headers = vec![header; MAX_HEADERS_RESULTS as usize + 1];

    let mut peers = connect_peers(Arc::new(data)).await;

    let result = call(
        &mut peers.initiator_client,
        Request::FindHeaders {
            known_blocks: Vec::new(),
            stop: None,
        },
    )
    .await;
    assert!(
        result.is_err(),
        "an over-sized get-headers response must fail the request, got: {result:?}",
    );

    let (_addr, points) = tokio::time::timeout(TEST_TIMEOUT, peers.initiator_misbehavior.recv())
        .await
        .expect("a misbehavior update arrives in time")
        .expect("the misbehavior channel is open");
    assert_eq!(points, MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED);

    // Scored violations do not disconnect: the peer is banned by the address
    // book once its accumulated score reaches the threshold.
    let response = call(&mut peers.initiator_client, Request::MempoolTransactionIds)
        .await
        .expect("the connection is still usable after a scored violation");
    assert!(
        matches!(response, Response::TransactionIds(_)),
        "got: {response:?}",
    );
}

#[tokio::test]
async fn v2_request_round_trips() {
    let _init_guard = zebra_test::init();

    let data = Arc::new(TestData::new());
    let mut peers = connect_peers(data.clone()).await;
    let initiator_client = &mut peers.initiator_client;

    // Ping is answered locally from the transport's RTT measurement.
    let response = call(initiator_client, Request::Ping(Default::default()))
        .await
        .expect("ping succeeds");
    assert!(matches!(response, Response::Pong(_)), "got: {response:?}");

    // FindHeaders maps to a get-headers request stream.
    let response = call(
        initiator_client,
        Request::FindHeaders {
            known_blocks: vec![data.headers[0].header.hash()],
            stop: None,
        },
    )
    .await
    .expect("find headers succeeds");
    match response {
        Response::BlockHeaders(headers) => {
            assert_eq!(headers.len(), 2);
            assert_eq!(headers[0].header, data.headers[0].header);
            assert_eq!(headers[1].header, data.headers[1].header);
        }
        other => panic!("expected BlockHeaders, got: {other:?}"),
    }

    // FindBlocks is bridged onto get-headers, returning header hashes.
    let response = call(
        initiator_client,
        Request::FindBlocks {
            known_blocks: vec![],
            stop: None,
        },
    )
    .await
    .expect("find blocks succeeds");
    match response {
        Response::BlockHashes(hashes) => {
            assert_eq!(
                hashes,
                vec![data.headers[0].header.hash(), data.headers[1].header.hash(),],
            );
        }
        other => panic!("expected BlockHashes, got: {other:?}"),
    }

    // BlocksByHash maps to a get-blocks request stream, with per-entry
    // found and not-found results.
    let block_hash = data.block.hash();
    let missing_hash = zebra_chain::block::Hash([0xEE; 32]);
    let response = call(
        initiator_client,
        Request::BlocksByHash([block_hash, missing_hash].into()),
    )
    .await
    .expect("blocks by hash succeeds");
    match response {
        Response::Blocks(blocks) => {
            assert_eq!(blocks.len(), 2);
            let available: Vec<_> = blocks
                .iter()
                .filter_map(|entry| entry.clone().available())
                .collect();
            assert_eq!(available.len(), 1);
            assert_eq!(available[0].0.hash(), block_hash);
            let missing: Vec<_> = blocks
                .into_iter()
                .filter_map(|entry| entry.missing())
                .collect();
            assert_eq!(missing, vec![missing_hash]);
        }
        other => panic!("expected Blocks, got: {other:?}"),
    }

    // TransactionsById maps to a get-tx request stream.
    let response = call(
        initiator_client,
        Request::TransactionsById([data.transaction.id].into()),
    )
    .await
    .expect("transactions by id succeeds");
    match response {
        Response::Transactions(transactions) => {
            assert_eq!(transactions.len(), 1);
            let (transaction, _) = transactions[0]
                .clone()
                .available()
                .expect("transaction is available");
            assert_eq!(transaction.id, data.transaction.id);
        }
        other => panic!("expected Transactions, got: {other:?}"),
    }

    // MempoolTransactionIds is answered from the connection's `get-mempool`
    // subscription mirror, so the responder's snapshot arrives
    // asynchronously after the handshake.
    let mempool_mirrored = async {
        loop {
            let response = call(initiator_client, Request::MempoolTransactionIds)
                .await
                .expect("mempool request succeeds");
            match response {
                Response::TransactionIds(ids) if ids == vec![data.transaction.id] => break,
                Response::TransactionIds(ids) => assert_eq!(ids, Vec::new()),
                other => panic!("expected TransactionIds, got: {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(TEST_TIMEOUT, mempool_mirrored)
        .await
        .expect("the mempool snapshot reaches the subscription mirror in time");

    // Peers maps to a get-addr request stream. The initiator made an
    // outbound connection, so the responder answers it. The responder's
    // announced listener address shares the cache with the served address,
    // and responses are random samples, so poll until the served address is
    // answered.
    let served_addr_answered = async {
        loop {
            let response = call(initiator_client, Request::Peers)
                .await
                .expect("peers request succeeds");
            match response {
                Response::Peers(peers) => {
                    if peers.iter().any(|peer| peer.addr() == data.peers[0].addr()) {
                        break;
                    }
                }
                other => panic!("expected Peers, got: {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(TEST_TIMEOUT, served_addr_answered)
        .await
        .expect("the served address is answered in time");
}

#[tokio::test]
async fn v2_get_addr_only_sent_on_self_initiated_connections() {
    let _init_guard = zebra_test::init();

    let data = Arc::new(TestData::new());
    let mut peers = connect_peers(data).await;

    // The initiator opened the connection, so the responder answers its
    // address requests with its served addresses. The response may also
    // contain the responder's announced listener address, which the
    // initiator caches as it arrives.
    let served: PeerSocketAddr = "203.0.113.7:8233".parse().expect("valid address");
    let served = crate::protocol::external::canonical_peer_addr(*served);

    let response = call(&mut peers.initiator_client, Request::Peers)
        .await
        .expect("get-addr on a self-initiated connection succeeds");
    match response {
        Response::Peers(addrs) => assert!(
            addrs.iter().any(|addr| addr.addr() == served),
            "expected the served address in the response, got: {addrs:?}",
        ),
        other => panic!("expected Peers, got: {other:?}"),
    }

    // The responder accepted an inbound connection, so a get-addr request to
    // the initiator would always be refused. It is not sent at all: the
    // responder answers locally with the addresses the initiator announced.
    // The initiator announces its own listener address after the handshake,
    // so the responder's local answer converges on it.
    let expected: PeerSocketAddr = "127.0.0.1:8233".parse().expect("valid address");
    let expected = crate::protocol::external::canonical_peer_addr(*expected);

    let local_answer_converges = async {
        loop {
            let response = call(&mut peers.responder_client, Request::Peers)
                .await
                .expect("get-addr on an inbound connection is answered locally");

            match response {
                Response::Peers(addrs) if addrs.iter().any(|addr| addr.addr() == expected) => {
                    break;
                }
                Response::Peers(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                other => panic!("expected Peers, got: {other:?}"),
            }
        }
    };
    tokio::time::timeout(TEST_TIMEOUT, local_answer_converges)
        .await
        .expect("the initiator's announced listener reaches the responder's cache in time");
}

#[tokio::test]
async fn v2_announcements_reach_the_remote_inbound_service() {
    let _init_guard = zebra_test::init();

    let data = Arc::new(TestData::new());
    let mut peers = connect_peers(data.clone()).await;
    let initiator_client = &mut peers.initiator_client;
    let responder_events = &mut peers.responder_events;

    // Advertising transaction IDs writes records on the transaction
    // announcement stream; the responder forwards them to its inbound
    // service.
    let response = call(
        initiator_client,
        Request::AdvertiseTransactionIds([data.transaction.id].into(), None),
    )
    .await
    .expect("advertise succeeds");
    assert!(matches!(response, Response::Nil));

    let event = tokio::time::timeout(TEST_TIMEOUT, responder_events.recv())
        .await
        .expect("announcement arrives in time")
        .expect("events channel is open");
    match event {
        Request::AdvertiseTransactionIds(ids, from) => {
            assert_eq!(ids, HashSet::from([data.transaction.id]));
            assert!(from.is_some(), "the announcing peer's address is recorded");
        }
        other => panic!("expected AdvertiseTransactionIds, got: {other:?}"),
    }

    // Advertising a block fetches it locally and announces its header; the
    // responder forwards the hash to its inbound service. Transaction
    // advertisements from the responder's own mempool subscription mirror
    // may interleave, and are skipped.
    let block_hash = data.block.hash();
    let response = call(initiator_client, Request::AdvertiseBlockToAll(block_hash))
        .await
        .expect("advertise succeeds");
    assert!(matches!(response, Response::Nil));

    let block_advertised = async {
        loop {
            let event = responder_events
                .recv()
                .await
                .expect("events channel is open");
            match event {
                Request::AdvertiseBlock(hash, from) => {
                    assert_eq!(hash, block_hash);
                    assert!(from.is_some(), "the announcing peer's address is recorded");
                    break;
                }
                Request::AdvertiseTransactionIds(_, _) => continue,
                other => panic!("expected AdvertiseBlock, got: {other:?}"),
            }
        }
    };
    tokio::time::timeout(TEST_TIMEOUT, block_advertised)
        .await
        .expect("announcement arrives in time");
}

#[tokio::test]
async fn v2_pushed_transactions_are_served_from_the_cache() {
    let _init_guard = zebra_test::init();

    let data = Arc::new(TestData::new());
    let mut peers = connect_peers(data.clone()).await;
    let ConnectedPeers {
        initiator_client,
        responder_client,
        responder_events,
        ..
    } = &mut peers;

    // Push a transaction the initiator's inbound service does NOT serve:
    // block 1's coinbase transaction.
    let pushed = UnminedTx::from(data.block.transactions[0].clone());
    assert_ne!(pushed.id, data.transaction.id);

    let response = call(
        initiator_client,
        Request::PushTransaction(pushed.clone(), None),
    )
    .await
    .expect("push succeeds");
    assert!(matches!(response, Response::Nil));

    // The push becomes an announcement to the responder. Advertisements
    // from the responder's mempool subscription mirror may interleave, and
    // are skipped.
    let push_advertised = async {
        loop {
            let event = responder_events
                .recv()
                .await
                .expect("events channel is open");
            match event {
                Request::AdvertiseTransactionIds(ref ids, _) if ids.contains(&pushed.id) => break,
                Request::AdvertiseTransactionIds(_, _) => continue,
                other => panic!("expected AdvertiseTransactionIds, got: {other:?}"),
            }
        }
    };
    tokio::time::timeout(TEST_TIMEOUT, push_advertised)
        .await
        .expect("announcement arrives in time");

    // The responder can fetch the pushed transaction from the initiator's
    // pushed-transaction cache, even though the initiator's inbound service
    // does not have it.
    let response = call(
        responder_client,
        Request::TransactionsById([pushed.id].into()),
    )
    .await
    .expect("fetching the pushed transaction succeeds");
    match response {
        Response::Transactions(transactions) => {
            assert_eq!(transactions.len(), 1);
            let (transaction, _) = transactions[0]
                .clone()
                .available()
                .expect("transaction is available");
            assert_eq!(transaction.id, pushed.id);
        }
        other => panic!("expected Transactions, got: {other:?}"),
    }
}

/// `get-blocks` requests are hash-only and served with full blocks, and
/// unservable `get-tx` references — `SHORTID` references without a sent
/// compact block, and references whose type does not match the referenced
/// transaction's version — are answered not-found, without an error or a
/// misbehavior penalty.
#[tokio::test]
async fn v2_get_blocks_is_hash_only_and_unservable_tx_refs_answer_not_found() {
    use zebra_chain::transaction::{AuthDigest, UnminedTxId, WtxId};

    use crate::protocol::v2::{
        compact_block::ShortTxId, record, request::Request as V2WireRequest,
        response::read_result_entry, txref::TransactionReference,
    };

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();
    let block = raw.data.block.clone();
    assert!(block.transactions.len() >= 3);

    // Request the block by hash on a raw request stream: `get-blocks`
    // requests carry only hashes, and are served with full blocks.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetBlocks {
            hashes: vec![block.hash()],
        },
    )
    .await;

    let entry = tokio::time::timeout(
        TEST_TIMEOUT,
        read_result_entry::<zebra_chain::block::Block, _>(&mut recv, "get-blocks"),
    )
    .await
    .expect("response arrives in time")
    .expect("response entry parses");
    match entry {
        Some(served) => assert_eq!(served.hash(), block.hash()),
        other => panic!("expected a full block response, got: {other:?}"),
    }
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    // `SHORTID` references resolve against the compact block most recently
    // sent to the requesting peer for the block. No compact block has been
    // sent on this connection, so they are answered not-found, without an
    // error or a penalty.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetTx {
            refs: vec![
                TransactionReference::ShortId {
                    block_hash: block.hash(),
                    short_id: ShortTxId([0x0F; 6]),
                },
                TransactionReference::ShortId {
                    block_hash: zebra_chain::block::Hash([0xAB; 32]),
                    short_id: ShortTxId([0x0F; 6]),
                },
            ],
        },
    )
    .await;

    for _ in 0..2 {
        let missing = tokio::time::timeout(
            TEST_TIMEOUT,
            read_result_entry::<zebra_chain::transaction::Transaction, _>(&mut recv, "get-tx"),
        )
        .await
        .expect("response arrives in time")
        .expect("response entry parses");
        assert!(
            missing.is_none(),
            "a SHORTID without a sent compact block is answered not-found",
        );
    }
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    // A `get-tx` reference whose type does not match the referenced
    // transaction's version is answered not-found: the requester may have
    // derived the reference type from data supplied by another peer, so it
    // must not be penalized. A correctly typed reference to the same
    // transaction is still served.
    let legacy_txid = match raw.data.transaction.id {
        UnminedTxId::Legacy(txid) => txid,
        other => panic!("the test mempool transaction is legacy, got: {other:?}"),
    };
    let wrong_typed = TransactionReference::Wtxid(WtxId {
        id: legacy_txid,
        auth_digest: AuthDigest([0xFF; 32]),
    });

    let mut recv = send_request(
        &connection,
        V2WireRequest::GetTx {
            refs: vec![
                wrong_typed,
                TransactionReference::from(raw.data.transaction.id),
            ],
        },
    )
    .await;

    let missing = tokio::time::timeout(
        TEST_TIMEOUT,
        read_result_entry::<zebra_chain::transaction::Transaction, _>(&mut recv, "get-tx"),
    )
    .await
    .expect("response arrives in time")
    .expect("response entry parses");
    assert!(
        missing.is_none(),
        "a wrong-typed reference is answered not-found",
    );
    let found = read_result_entry::<zebra_chain::transaction::Transaction, _>(&mut recv, "get-tx")
        .await
        .expect("response entry parses");
    match found {
        Some(transaction) => {
            assert_eq!(transaction, raw.data.transaction.transaction);
        }
        other => panic!("expected the referenced transaction, got: {other:?}"),
    }
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    // None of the not-found answers above assigned a misbehavior penalty.
    assert!(
        raw.responder_misbehavior.try_recv().is_err(),
        "unservable get-tx references must not be penalized",
    );
}

/// A raw responder connected to a full v2 initiator stack: the full stack
/// initiated the connection, and the test controls the responder's streams.
struct RawResponder {
    /// The test data the initiator stack's inbound service serves.
    data: Arc<TestData>,

    /// The raw responder's QUIC connection, for raw streams.
    connection: quinn::Connection,

    /// The raw responder's completed handshake, carrying the initiator
    /// stack's `init` record.
    handshake: crate::peer::v2::handshake::V2Handshake,

    /// The initiator stack's peer client for this connection.
    initiator_client: Client,

    /// The gossip requests the initiator stack's inbound service received.
    initiator_events: tokio::sync::mpsc::UnboundedReceiver<Request>,

    /// The misbehavior scores the initiator stack assigned to the raw
    /// responder.
    initiator_misbehavior: tokio::sync::mpsc::Receiver<(PeerSocketAddr, u32)>,
}

/// Builds a full v2 stack and connects it *outbound*, to a raw responder
/// that completes only the handshake, leaving the test in control of the
/// responder side of every stream.
async fn raw_responder(data: Arc<TestData>) -> RawResponder {
    use crate::{
        peer::{
            handshake::HandshakeNonces,
            v2::handshake::{respond, HandshakeParams},
        },
        protocol::{
            external::types::Nonce,
            v2::{constants::MIN_V2_PROTOCOL_VERSION, init::InitRecord},
        },
    };

    let network = Network::Mainnet;
    let (responder_endpoint, responder_addr) = test_endpoint(&network);

    let (initiator_misbehavior_tx, initiator_misbehavior) = tokio::sync::mpsc::channel(100);
    let (mut initiator_handshaker, initiator_events) =
        test_handshaker(&network, data.clone(), initiator_misbehavior_tx);

    let mut counter = ActiveConnectionCounter::new_counter();
    let initiator_tracker = counter.track_connection();
    let initiator_endpoint =
        quic::new_endpoint("127.0.0.1:0".parse().expect("valid address"), &network)
            .expect("endpoint creation succeeds");
    let initiator_task = tokio::spawn(async move {
        let connection = quic::connect(&initiator_endpoint, responder_addr, &network)
            .await
            .expect("outbound connection completes");
        let connected_addr = crate::peer::ConnectedAddr::new_outbound_direct(responder_addr.into());

        let client = initiator_handshaker
            .ready()
            .await
            .expect("handshaker is ready")
            .call(V2HandshakeRequest {
                connection,
                connected_addr,
                connection_tracker: initiator_tracker,
            })
            .await
            .expect("initiator handshake succeeds");

        std::mem::forget(initiator_endpoint);
        client
    });

    let incoming = responder_endpoint.accept().await.expect("endpoint accepts");
    let connection = quic::accept(incoming, &Network::Mainnet)
        .await
        .expect("inbound connection completes");
    std::mem::forget(responder_endpoint);

    let params = HandshakeParams {
        local_init: InitRecord {
            version: MIN_V2_PROTOCOL_VERSION,
            services: PeerServices::NODE_NETWORK,
            nonce: Nonce::default(),
            user_agent: "/ZebraV2RawTest:0.0.1/".to_string(),
            start_height: zebra_chain::block::Height(0),
            relay: true,
            announce: false,
            full_ids: false,
        },
        min_remote_version: MIN_V2_PROTOCOL_VERSION,
        nonces: HandshakeNonces::default(),
        nonce_limit: 100,
    };
    let handshake = tokio::time::timeout(TEST_TIMEOUT, respond(&connection, &params))
        .await
        .expect("handshake completes in time")
        .expect("responder handshake succeeds");
    let initiator_client = initiator_task.await.expect("initiator task succeeds");

    RawResponder {
        data,
        connection,
        handshake,
        initiator_client,
        initiator_events,
        initiator_misbehavior,
    }
}

/// A compact block announcement is reconstructed by the receiving node: it
/// requests the transactions it is missing with `SHORTID` `get-tx`
/// references, assembles and merkle-checks the block, hands it to its
/// gossip pipeline with the advertiser withheld (the high-bandwidth penalty
/// exemption), and serves the follow-up block request from the connection's
/// cache without a wire request.
#[tokio::test]
async fn v2_compact_announcement_is_reconstructed_and_served_from_cache() {
    use crate::protocol::v2::{
        compact_block::{CompactBlock, CompactBlockIds},
        record,
        request::Request as V2WireRequest,
        response::encode_result_entry,
        txref::TransactionReference,
        types::StreamType,
    };

    let _init_guard = zebra_test::init();

    let mut raw = raw_responder(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();
    let block = raw.data.block.clone();

    // The full stack initiated this connection, so it claimed a
    // high-bandwidth slot and requested compact block announcements with
    // short transaction IDs.
    assert!(
        raw.handshake.remote_init.announce,
        "self-initiated connections request high-bandwidth announcements",
    );
    assert!(!raw.handshake.remote_init.full_ids);

    // Announce the multi-transaction block as a compact block. The stack's
    // mempool does not hold its transactions, so it must request every
    // non-coinbase transaction (the coinbase is prefilled).
    let compact = CompactBlock::from_block(&block, 7, false, &[]).expect("compact block builds");
    let mut payload = vec![0x01];
    compact.encode(&mut payload).expect("compact block encodes");
    let mut announcement = Vec::new();
    record::write_record(&mut announcement, &payload).expect("record writes");

    let mut send = connection.open_uni().await.expect("stream opens");
    send.write_all(&[StreamType::BlockAnnouncements.byte()])
        .await
        .expect("stream type writes");
    send.write_all(&announcement).await.expect("record writes");

    // Serve the reconstruction `get-tx`: one `SHORTID` reference per
    // non-coinbase transaction, in block order. The stack's standing
    // `get-mempool` subscription stream is skipped unanswered.
    let (mut req_send, mut req_recv) = tokio::time::timeout(
        TEST_TIMEOUT,
        accept_request_stream(&connection, StreamType::GetTx),
    )
    .await
    .expect("get-tx stream opens in time");

    let request = V2WireRequest::read(StreamType::GetTx, &mut req_recv)
        .await
        .expect("get-tx request parses");
    let V2WireRequest::GetTx { refs } = request else {
        panic!("expected a get-tx request, got: {request:?}");
    };
    let CompactBlockIds::Short(short_ids) = &compact.ids else {
        panic!("the announced compact block has short IDs");
    };
    let expected_refs: Vec<TransactionReference> = short_ids
        .iter()
        .map(|short_id| TransactionReference::ShortId {
            block_hash: block.hash(),
            short_id: *short_id,
        })
        .collect();
    assert_eq!(refs, expected_refs);

    let mut bytes = Vec::new();
    for tx in block.transactions.iter().skip(1) {
        encode_result_entry(&mut bytes, Some(tx.as_ref())).expect("entry encodes");
    }
    req_send.write_all(&bytes).await.expect("response writes");
    req_send.finish().expect("stream finishes");

    // The stack reconstructs the block and forwards the announcement with
    // the advertiser withheld: a high-bandwidth announcement of a block
    // that fails validation must not be penalized.
    let event = tokio::time::timeout(TEST_TIMEOUT, raw.initiator_events.recv())
        .await
        .expect("announcement forwarded in time")
        .expect("events channel is open");
    match event {
        Request::AdvertiseBlock(hash, advertiser) => {
            assert_eq!(hash, block.hash());
            assert!(
                advertiser.is_none(),
                "high-bandwidth announcements withhold the advertiser",
            );
        }
        other => panic!("expected AdvertiseBlock, got: {other:?}"),
    }

    // The reconstructed block serves the follow-up block request from the
    // connection's cache. This test never serves `get-blocks`, so a wire
    // request would hang past this timeout.
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        call(
            &mut raw.initiator_client,
            Request::BlocksByHash([block.hash()].into()),
        ),
    )
    .await
    .expect("the cached block answers immediately")
    .expect("block request succeeds");
    match response {
        Response::Blocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            let (served, _) = blocks[0].clone().available().expect("block is available");
            assert_eq!(served, block);
        }
        other => panic!("expected Blocks, got: {other:?}"),
    }

    // Reconstruction assigned no misbehavior penalty.
    assert!(raw.initiator_misbehavior.try_recv().is_err());
}

/// Serves one `get-block-range` request on `connection` with `result` and
/// the given blocks, and asserts the request's anchor hash.
async fn serve_block_range(
    connection: &quinn::Connection,
    expected_final_hash: zebra_chain::block::Hash,
    result: u8,
    blocks: Vec<Arc<Block>>,
) {
    use zebra_chain::serialization::ZcashSerialize;

    use crate::protocol::v2::{record, request::Request as V2WireRequest, types::StreamType};

    let (mut send, mut recv) = accept_request_stream(connection, StreamType::GetBlockRange).await;
    let request = V2WireRequest::read(StreamType::GetBlockRange, &mut recv)
        .await
        .expect("request parses");
    let V2WireRequest::GetBlockRange { final_hash, .. } = request else {
        panic!("expected a get-block-range request, got: {request:?}");
    };
    assert_eq!(final_hash, expected_final_hash);

    let mut bytes = vec![result];
    for block in &blocks {
        let block_bytes = block
            .zcash_serialize_to_vec()
            .expect("serializing a block succeeds");
        record::write_record(&mut bytes, &block_bytes).expect("record writes");
    }
    send.write_all(&bytes).await.expect("response writes");
    send.finish().expect("stream finishes");
}

/// The block-range requester verifies every delivered block on arrival and
/// returns them in descending order; a peer without the anchor answers a
/// missing entry.
#[tokio::test]
async fn v2_block_range_requester_returns_verified_blocks() {
    let _init_guard = zebra_test::init();

    let mut raw = raw_responder(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();
    let block_1 = raw.data.block_1.clone();
    let block_2 = raw.data.block_2.clone();

    let serve = serve_block_range(
        &connection,
        block_2.hash(),
        0x00,
        vec![block_2.clone(), block_1.clone()],
    );
    let request = call(
        &mut raw.initiator_client,
        Request::BlockRange {
            final_hash: block_2.hash(),
            count: 5,
            max_bytes: 8_000_000,
        },
    );
    let (_, response) = tokio::join!(serve, tokio::time::timeout(TEST_TIMEOUT, request));
    let response = response
        .expect("response arrives in time")
        .expect("block range request succeeds");
    match response {
        Response::Blocks(blocks) => {
            let hashes: Vec<_> = blocks
                .into_iter()
                .map(|entry| entry.available().expect("blocks are available").0.hash())
                .collect();
            assert_eq!(hashes, vec![block_2.hash(), block_1.hash()]);
        }
        other => panic!("expected Blocks, got: {other:?}"),
    }

    // A peer without the anchor block answers a missing entry.
    let missing_hash = zebra_chain::block::Hash([0xEE; 32]);
    let serve = serve_block_range(&connection, missing_hash, 0x02, Vec::new());
    let request = call(
        &mut raw.initiator_client,
        Request::BlockRange {
            final_hash: missing_hash,
            count: 5,
            max_bytes: 8_000_000,
        },
    );
    let (_, response) = tokio::join!(serve, tokio::time::timeout(TEST_TIMEOUT, request));
    let response = response
        .expect("response arrives in time")
        .expect("block range request succeeds");
    match response {
        Response::Blocks(blocks) => {
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].clone().missing(), Some(missing_hash));
        }
        other => panic!("expected Blocks, got: {other:?}"),
    }
}

/// A response that breaks a `get-block-range` rule fails the request and
/// the connection.
///
/// The rules are checkable by hashing alone, so a violation is never
/// attributable to a different chain view: verification failures are
/// `PROTOCOL_ERROR`, and exceeding a request bound is `FLOOD`.
#[tokio::test]
async fn v2_block_range_requester_rejects_invalid_responses() {
    use crate::protocol::v2::types::ErrorCode;

    let _init_guard = zebra_test::init();

    let data = TestData::new();
    let block_1 = data.block_1.clone();
    let block_2 = data.block_2.clone();

    // Block 2's header with block 1's transactions: it hashes to the
    // anchor, but its transactions do not match its merkle root.
    let wrong_merkle_root = Arc::new(Block {
        header: block_2.header.clone(),
        transactions: block_1.transactions.clone(),
    });

    let cases = [
        (
            "a block that does not hash to the anchor",
            vec![block_1.clone()],
            5,
            ErrorCode::ProtocolError,
        ),
        (
            "a block that does not match its merkle root",
            vec![wrong_merkle_root],
            5,
            ErrorCode::ProtocolError,
        ),
        (
            "more blocks than the request asked for",
            vec![block_2.clone(), block_1],
            1,
            ErrorCode::Flood,
        ),
    ];

    for (rule, blocks, count, expected) in cases {
        let mut raw = raw_responder(Arc::new(TestData::new())).await;
        let connection = raw.connection.clone();

        let serve = serve_block_range(&connection, block_2.hash(), 0x00, blocks);
        let request = call(
            &mut raw.initiator_client,
            Request::BlockRange {
                final_hash: block_2.hash(),
                count,
                max_bytes: 8_000_000,
            },
        );

        let (_, response) = tokio::join!(serve, tokio::time::timeout(TEST_TIMEOUT, request));
        assert!(
            response.expect("response arrives in time").is_err(),
            "{rule} must fail the request",
        );

        assert_closed_with(&connection, expected, rule).await;
    }
}

/// The relayed-address token bucket admits a full burst, then throttles to
/// the reference rate.
#[test]
fn v2_addr_token_bucket_bounds_the_rate() {
    use std::time::{Duration, Instant};

    use crate::{
        peer::v2::connection::AddrTokenBucket,
        protocol::v2::constants::{ADDR_TOKEN_BUCKET_CAPACITY, ADDR_TOKEN_RATE},
    };

    let _init_guard = zebra_test::init();

    let mut bucket = AddrTokenBucket::default();
    let start = Instant::now();

    // The full burst is admitted, and the next address is not.
    for _ in 0..ADDR_TOKEN_BUCKET_CAPACITY as usize {
        assert!(bucket.try_take(start));
    }
    assert!(!bucket.try_take(start));

    // One token refills after 1/rate seconds.
    let refill = start + Duration::from_secs_f64(1.0 / ADDR_TOKEN_RATE);
    assert!(bucket.try_take(refill));
    assert!(!bucket.try_take(refill));

    // Refill never exceeds the capacity.
    let much_later = start + Duration::from_secs(60 * 60 * 24 * 365);
    for _ in 0..ADDR_TOKEN_BUCKET_CAPACITY as usize {
        assert!(bucket.try_take(much_later));
    }
    assert!(!bucket.try_take(much_later));
}

/// Sends `request` on a new request stream, and returns the response
/// stream.
async fn send_request(
    connection: &quinn::Connection,
    request: crate::protocol::v2::request::Request,
) -> quinn::RecvStream {
    let (mut send, recv) = connection.open_bi().await.expect("stream opens");
    let mut bytes = vec![request.stream_type().byte()];
    request.encode(&mut bytes).expect("request encodes");
    send.write_all(&bytes).await.expect("request writes");
    send.finish().expect("stream finishes");
    recv
}

/// Reads one `get-mempool` subscription record from `recv`.
async fn read_mempool_record(
    recv: &mut quinn::RecvStream,
) -> Vec<crate::protocol::v2::txref::TransactionReference> {
    use crate::protocol::v2::{record, response::MempoolResponse};

    let payload = tokio::time::timeout(TEST_TIMEOUT, record::read_record(recv))
        .await
        .expect("record arrives in time")
        .expect("record parses")
        .expect("record is present");
    let mut reader = payload.as_slice();
    let response = MempoolResponse::read(&mut reader)
        .await
        .expect("record payload parses");
    assert!(reader.is_empty(), "no trailing bytes in the record");
    response.0
}

/// Asserts that `connection` was closed with the application error `code`.
async fn assert_closed_with(
    connection: &quinn::Connection,
    code: crate::protocol::v2::types::ErrorCode,
    message: &str,
) {
    use crate::protocol::v2::types::ErrorCode;

    let closed = tokio::time::timeout(TEST_TIMEOUT, connection.closed())
        .await
        .expect("connection closes in time");
    match closed {
        quinn::ConnectionError::ApplicationClosed(closed) => assert_eq!(
            ErrorCode::from_wire(closed.error_code.into()),
            code,
            "{message}",
        ),
        other => panic!("expected an application close, got: {other:?}"),
    }
}

/// Asserts that the peer reset `recv` with the application error `code`.
async fn assert_reset_with(
    recv: &mut quinn::RecvStream,
    code: crate::protocol::v2::types::ErrorCode,
    message: &str,
) {
    use crate::protocol::v2::types::ErrorCode;

    let mut buf = [0u8; 1];
    let result = tokio::time::timeout(TEST_TIMEOUT, recv.read(&mut buf))
        .await
        .expect("the reset arrives in time");
    match result {
        Err(quinn::ReadError::Reset(reset_code)) => assert_eq!(
            ErrorCode::from_wire(reset_code.into_inner()),
            code,
            "{message}",
        ),
        other => panic!("expected a stream reset, got: {other:?}"),
    }
}

/// Accepts bidirectional streams on `connection` until the peer opens one
/// of `stream_type`, and returns it with the stream type byte consumed.
///
/// Streams of other types — like the peer's standing `get-mempool`
/// subscription — are dropped unanswered.
async fn accept_request_stream(
    connection: &quinn::Connection,
    stream_type: crate::protocol::v2::types::StreamType,
) -> (quinn::SendStream, quinn::RecvStream) {
    loop {
        let (send, mut recv) = connection.accept_bi().await.expect("bi stream accepted");
        let mut type_byte = [0u8; 1];
        recv.read_exact(&mut type_byte)
            .await
            .expect("stream type byte is present");
        if type_byte[0] == stream_type.byte() {
            return (send, recv);
        }
    }
}

/// `get-mempool` opens a subscription: the responder sends its mempool
/// snapshot, keeps its sending direction open, references transactions as
/// they are accepted into its mempool, and serves a fresh snapshot to a
/// sequential re-subscription after the first is cancelled.
#[tokio::test]
async fn v2_get_mempool_subscription_streams_snapshot_and_updates() {
    use zebra_chain::transaction::UnminedTx;

    use crate::protocol::v2::{
        request::Request as V2WireRequest, txref::TransactionReference, types::ErrorCode,
    };

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    // Subscribe: the snapshot references the responder's whole mempool.
    let mut recv = send_request(&connection, V2WireRequest::GetMempool).await;

    let snapshot = read_mempool_record(&mut recv).await;
    assert_eq!(
        snapshot,
        vec![TransactionReference::from(raw.data.transaction.id)],
    );

    // A transaction accepted into the responder's mempool afterwards is
    // referenced in a further record on the same stream.
    let accepted = UnminedTx::from(raw.data.block.transactions[1].clone());
    call(
        &mut raw.responder_client,
        Request::AdvertiseTransactionIds([accepted.id].into(), None),
    )
    .await
    .expect("advertise succeeds");

    let update = read_mempool_record(&mut recv).await;
    assert_eq!(update, vec![TransactionReference::from(accepted.id)]);

    // Cancelling the subscription releases its slot: a sequential
    // re-subscription is served a fresh snapshot.
    recv.stop(ErrorCode::Cancelled.into())
        .expect("stop succeeds");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut recv = send_request(&connection, V2WireRequest::GetMempool).await;

    let snapshot = read_mempool_record(&mut recv).await;
    assert_eq!(
        snapshot,
        vec![TransactionReference::from(raw.data.transaction.id)],
        "a sequential re-subscription is served again",
    );

    // Subscription serving assigns no misbehavior penalty.
    assert!(raw.responder_misbehavior.try_recv().is_err());
}

/// Transaction announcements are trickled: identifiers advertised together
/// are flushed as one batch — a single `get-mempool` update record — and
/// each is also announced on the transaction announcement stream.
#[tokio::test]
async fn v2_transaction_announcements_are_trickled_in_batches() {
    use std::collections::HashSet;

    use zebra_chain::transaction::UnminedTx;

    use crate::protocol::v2::{
        record, request::Request as V2WireRequest, txref::TransactionReference, types::StreamType,
    };

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    // Subscribe first, so the trickled update record can be observed.
    let mut recv = send_request(&connection, V2WireRequest::GetMempool).await;
    let snapshot = read_mempool_record(&mut recv).await;
    assert_eq!(
        snapshot,
        vec![TransactionReference::from(raw.data.transaction.id)],
    );

    // Two transactions advertised together share a trickle batch.
    let tx_a = UnminedTx::from(raw.data.block.transactions[1].clone());
    let tx_b = UnminedTx::from(raw.data.block.transactions[2].clone());
    call(
        &mut raw.responder_client,
        Request::AdvertiseTransactionIds([tx_a.id, tx_b.id].into(), None),
    )
    .await
    .expect("advertise succeeds");

    let update: HashSet<TransactionReference> =
        read_mempool_record(&mut recv).await.into_iter().collect();
    let expected: HashSet<TransactionReference> = [
        TransactionReference::from(tx_a.id),
        TransactionReference::from(tx_b.id),
    ]
    .into();
    assert_eq!(
        update, expected,
        "a batch advertised together is flushed as one update record",
    );

    // Each identifier is also announced on the announcement stream.
    let mut announcements = tokio::time::timeout(
        TEST_TIMEOUT,
        accept_announcement_stream(&connection, StreamType::TransactionAnnouncements),
    )
    .await
    .expect("transaction announcement stream opens in time");

    let mut announced = HashSet::new();
    for _ in 0..2 {
        let payload = tokio::time::timeout(TEST_TIMEOUT, record::read_record(&mut announcements))
            .await
            .expect("announcement arrives in time")
            .expect("announcement record parses")
            .expect("announcement record is present");
        announced.insert(
            TransactionReference::parse_exact(&payload)
                .await
                .expect("announcement reference parses"),
        );
    }
    assert_eq!(announced, expected);
}

/// A second concurrent `get-mempool` subscription from the same peer is a
/// connection error of type `PROTOCOL_ERROR`.
#[tokio::test]
async fn v2_second_concurrent_mempool_subscription_is_a_connection_error() {
    use crate::protocol::v2::types::{ErrorCode, StreamType};

    let _init_guard = zebra_test::init();

    let raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    // Both receive halves stay open: dropping one would cancel that
    // subscription, making the second one sequential instead of concurrent.
    let (mut first, _first_recv) = connection.open_bi().await.expect("stream opens");
    first
        .write_all(&[StreamType::GetMempool.byte()])
        .await
        .expect("stream type writes");
    first.finish().expect("stream finishes");

    let (mut second, _second_recv) = connection.open_bi().await.expect("stream opens");
    second
        .write_all(&[StreamType::GetMempool.byte()])
        .await
        .expect("stream type writes");
    second.finish().expect("stream finishes");

    assert_closed_with(
        &connection,
        ErrorCode::ProtocolError,
        "a second concurrent get-mempool subscription is a connection error",
    )
    .await;
}

/// Accepts unidirectional streams on `connection` until the peer opens one
/// of `stream_type`, and returns it with the stream type byte consumed.
async fn accept_announcement_stream(
    connection: &quinn::Connection,
    stream_type: crate::protocol::v2::types::StreamType,
) -> quinn::RecvStream {
    loop {
        let mut recv = connection.accept_uni().await.expect("uni stream accepted");
        let mut type_byte = [0u8; 1];
        recv.read_exact(&mut type_byte)
            .await
            .expect("stream type byte is present");
        if type_byte[0] == stream_type.byte() {
            return recv;
        }
    }
}

/// A block advertised to a peer that requested high-bandwidth announcements
/// is announced as a compact block, and the announcing node then serves the
/// block's transactions to that peer's reconstruction `get-tx` requests: by
/// `SHORTID` reference, and by ordinary reference even for transactions
/// that are not in its mempool.
#[tokio::test]
async fn v2_hb_compact_block_announcement_and_reconstruction_serving() {
    use crate::protocol::v2::{
        compact_block::{short_id_keys, short_transaction_id, CompactBlock, CompactBlockIds},
        record,
        request::Request as V2WireRequest,
        response::read_result_entry,
        txref::TransactionReference,
        types::StreamType,
    };
    use zebra_chain::{serialization::ZcashSerialize, transaction::UnminedTxId};

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator_with_init(Arc::new(TestData::new()), true, false).await;
    let connection = raw.connection.clone();
    let block = raw.data.block.clone();
    assert!(block.transactions.len() >= 3);

    // The responder node advertises the multi-transaction block: it fetches
    // the block locally and announces it to this high-bandwidth peer.
    call(
        &mut raw.responder_client,
        Request::AdvertiseBlockToAll(block.hash()),
    )
    .await
    .expect("advertise request succeeds");

    let mut announcements = tokio::time::timeout(
        TEST_TIMEOUT,
        accept_announcement_stream(&connection, StreamType::BlockAnnouncements),
    )
    .await
    .expect("block announcement stream opens in time");

    let payload = tokio::time::timeout(TEST_TIMEOUT, record::read_record(&mut announcements))
        .await
        .expect("announcement arrives in time")
        .expect("announcement record parses")
        .expect("announcement record is present");
    let (&kind, mut body) = payload
        .split_first()
        .expect("announcement record is not empty");
    assert_eq!(kind, 0x01, "high-bandwidth peers get compact blocks");

    let compact = CompactBlock::read(&mut body)
        .await
        .expect("compact block parses");
    assert_eq!(compact.header.hash(), block.hash());

    // The coinbase transaction is prefilled, and every other transaction is
    // identified by a short ID computed with the announcement's nonce.
    assert_eq!(compact.prefilled.len(), 1);
    assert_eq!(compact.prefilled[0].index, 0);
    assert_eq!(compact.prefilled[0].tx, block.transactions[0]);

    let header_bytes = block
        .header
        .zcash_serialize_to_vec()
        .expect("serializing a header succeeds");
    let (k0, k1) = short_id_keys(&header_bytes, compact.nonce);
    let expected_ids: Vec<_> = block
        .transactions
        .iter()
        .skip(1)
        .map(|tx| short_transaction_id(k0, k1, &UnminedTxId::from(tx.as_ref())))
        .collect();
    let CompactBlockIds::Short(ids) = &compact.ids else {
        panic!("a peer that did not request full IDs gets short IDs");
    };
    assert_eq!(ids, &expected_ids);

    // The block's transactions are now servable to this peer: by the short
    // IDs of the announcement, and by ordinary references, even though the
    // responder's mempool does not hold them. Unknown short IDs and other
    // blocks' short IDs are answered not-found.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetTx {
            refs: vec![
                TransactionReference::ShortId {
                    block_hash: block.hash(),
                    short_id: expected_ids[0],
                },
                TransactionReference::from(UnminedTxId::from(block.transactions[2].as_ref())),
                TransactionReference::ShortId {
                    block_hash: block.hash(),
                    short_id: crate::protocol::v2::compact_block::ShortTxId([0x0F; 6]),
                },
                TransactionReference::ShortId {
                    block_hash: zebra_chain::block::Hash([0xAB; 32]),
                    short_id: expected_ids[0],
                },
            ],
        },
    )
    .await;

    let mut entries = Vec::new();
    for _ in 0..4 {
        let entry = tokio::time::timeout(
            TEST_TIMEOUT,
            read_result_entry::<zebra_chain::transaction::Transaction, _>(&mut recv, "get-tx"),
        )
        .await
        .expect("response arrives in time")
        .expect("response entry parses");
        entries.push(entry);
    }
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    match &entries[0] {
        Some(tx) => assert_eq!(tx, &block.transactions[1]),
        other => panic!("expected the short ID's transaction, got: {other:?}"),
    }
    match &entries[1] {
        Some(tx) => assert_eq!(tx, &block.transactions[2]),
        other => panic!("expected the sent block's transaction, got: {other:?}"),
    }
    assert!(
        entries[2].is_none(),
        "an unknown short ID is answered not-found",
    );
    assert!(
        entries[3].is_none(),
        "a short ID for a block without a sent compact block is answered not-found",
    );

    // High-bandwidth serving assigns no misbehavior penalty.
    assert!(raw.responder_misbehavior.try_recv().is_err());
}

/// A high-bandwidth peer that also requested full transaction IDs receives
/// compact blocks whose transactions are identified by 64-byte full IDs,
/// with a zero nonce.
#[tokio::test]
async fn v2_hb_compact_block_uses_full_ids_on_request() {
    use crate::protocol::v2::{
        compact_block::{full_transaction_id, CompactBlock, CompactBlockIds},
        record,
        types::StreamType,
    };
    use zebra_chain::transaction::UnminedTxId;

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator_with_init(Arc::new(TestData::new()), true, true).await;
    let connection = raw.connection.clone();
    let block = raw.data.block.clone();

    call(
        &mut raw.responder_client,
        Request::AdvertiseBlockToAll(block.hash()),
    )
    .await
    .expect("advertise request succeeds");

    let mut announcements = tokio::time::timeout(
        TEST_TIMEOUT,
        accept_announcement_stream(&connection, StreamType::BlockAnnouncements),
    )
    .await
    .expect("block announcement stream opens in time");

    let payload = tokio::time::timeout(TEST_TIMEOUT, record::read_record(&mut announcements))
        .await
        .expect("announcement arrives in time")
        .expect("announcement record parses")
        .expect("announcement record is present");
    let (&kind, mut body) = payload
        .split_first()
        .expect("announcement record is not empty");
    assert_eq!(kind, 0x01);

    let compact = CompactBlock::read(&mut body)
        .await
        .expect("compact block parses");
    assert_eq!(compact.header.hash(), block.hash());
    assert_eq!(compact.nonce, 0, "full ID compact blocks have a zero nonce");

    let expected_ids: Vec<_> = block
        .transactions
        .iter()
        .skip(1)
        .map(|tx| full_transaction_id(&UnminedTxId::from(tx.as_ref())))
        .collect();
    let CompactBlockIds::Full(ids) = &compact.ids else {
        panic!("a peer that requested full IDs gets full IDs");
    };
    assert_eq!(ids, &expected_ids);
}

/// Block announcements are built as headers for peers that did not request
/// compact blocks, and as the header substitute when a block's compact
/// encoding would exceed the record payload limit.
#[test]
fn v2_block_announcements_fall_back_to_headers() {
    use crate::{
        peer::v2::connection::{build_block_announcement, BlockAnnouncementRecord},
        protocol::{
            external::types::{Nonce, Version},
            v2::init::InitRecord,
        },
    };
    use zebra_chain::{block::Header, serialization::ZcashDeserialize};

    let _init_guard = zebra_test::init();

    let data = TestData::new();
    let init = |announce, full_ids| InitRecord {
        version: Version(170_160),
        services: PeerServices::NODE_NETWORK,
        nonce: Nonce(1),
        user_agent: "/ZebraV2Test:0.0.1/".to_string(),
        start_height: zebra_chain::block::Height(0),
        relay: true,
        announce,
        full_ids,
    };

    // A low-bandwidth peer gets a header announcement.
    match build_block_announcement(&data.block, &init(false, false), 42) {
        BlockAnnouncementRecord::Header { payload } => {
            assert_eq!(payload[0], 0x00);
            let header = Header::zcash_deserialize(&payload[1..]).expect("header parses");
            assert_eq!(header.hash(), data.block.hash());
        }
        BlockAnnouncementRecord::Compact { .. } => {
            panic!("low-bandwidth peers must not get compact blocks")
        }
    }

    // A high-bandwidth peer gets a compact block, carrying the nonce.
    match build_block_announcement(&data.block, &init(true, false), 42) {
        BlockAnnouncementRecord::Compact { payload, nonce } => {
            assert_eq!(payload[0], 0x01);
            assert_eq!(nonce, 42);
        }
        BlockAnnouncementRecord::Header { .. } => {
            panic!("high-bandwidth peers get compact blocks")
        }
    }

    // A block whose compact encoding exceeds the record payload limit is
    // announced with the header substitute: 35,000 transactions at 64 bytes
    // of full ID each overflow the 2 MiB record limit.
    let huge_block = Block {
        header: data.block.header.clone(),
        transactions: vec![data.block.transactions[1].clone(); 35_000],
    };
    match build_block_announcement(&huge_block, &init(true, true), 42) {
        BlockAnnouncementRecord::Header { payload } => assert_eq!(payload[0], 0x00),
        BlockAnnouncementRecord::Compact { .. } => {
            panic!("an oversized compact block must fall back to a header announcement")
        }
    }
}

/// `get-headers` with `tx_ids = 1` identifies each served block's
/// transactions: the coinbase in full, and the rest by their full
/// transaction IDs. Blocks the responder cannot supply are answered with
/// `has_txs = 0x00`.
#[tokio::test]
async fn v2_get_headers_with_tx_ids_serves_coinbase_and_full_ids() {
    use tokio::io::AsyncReadExt;

    use zebra_chain::{block::Header, transaction::Transaction};

    use crate::protocol::v2::{record, request::Request as V2WireRequest, types::WireError};

    let _init_guard = zebra_test::init();

    // Header 2's block is unavailable, so the has_txs = 0 fallback is
    // exercised.
    let mut data = TestData::new();
    data.served_blocks.shift_remove(&data.block_2.hash());
    let raw = raw_initiator(Arc::new(data)).await;
    let block_1 = raw.data.block_1.clone();

    let mut recv = send_request(
        &raw.connection,
        V2WireRequest::GetHeaders {
            known_blocks: vec![],
            stop: None,
            tx_ids: true,
        },
    )
    .await;

    let response = tokio::time::timeout(TEST_TIMEOUT, async {
        let count = record::read_compact_size(&mut recv).await?;

        let mut entries = Vec::new();
        for _ in 0..count {
            let header_bytes = record::read_record(&mut recv)
                .await?
                .expect("header record is present");
            let header: Header = header_bytes
                .as_slice()
                .zcash_deserialize_into()
                .expect("served header parses");

            let txs = match recv.read_u8().await.expect("has_txs byte is present") {
                0x00 => None,
                0x01 => {
                    let coinbase_bytes = record::read_record(&mut recv)
                        .await?
                        .expect("coinbase record is present");
                    let coinbase: Transaction = coinbase_bytes
                        .as_slice()
                        .zcash_deserialize_into()
                        .expect("served coinbase parses");

                    let id_count = record::read_compact_size(&mut recv).await?;
                    let mut ids = Vec::new();
                    for _ in 0..id_count {
                        let mut id = [0u8; 64];
                        recv.read_exact(&mut id).await.expect("full ID is present");
                        ids.push(id);
                    }

                    Some((coinbase, ids))
                }
                other => panic!("invalid has_txs byte: {other:#04X}"),
            };

            entries.push((header, txs));
        }

        record::expect_end_of_stream(&mut recv).await?;
        Ok::<_, WireError>(entries)
    })
    .await
    .expect("response arrives in time")
    .expect("response parses");

    assert_eq!(response.len(), raw.data.headers.len());

    // Block 1 is served: its entry carries the coinbase transaction and the
    // (empty) list of the remaining transactions' full IDs.
    let (header, txs) = &response[0];
    assert_eq!(header.hash(), block_1.hash());
    let (coinbase, ids) = txs.as_ref().expect("block 1 is served with transactions");
    assert_eq!(coinbase, block_1.transactions[0].as_ref());
    assert!(ids.is_empty(), "block 1 has only a coinbase transaction");

    // Block 2 is not available from the responder's inbound service, so its
    // transactions are omitted: the requester falls back to `get-blocks`.
    let (header, txs) = &response[1];
    assert_eq!(header.hash(), raw.data.headers[1].header.hash());
    assert!(
        txs.is_none(),
        "an unavailable block is answered has_txs = 0"
    );

    // Sending the entry obligates the responder to serve the block's
    // transactions to this peer, even though they are not in its mempool —
    // but not by `SHORTID`, which only a compact block establishes.
    use crate::protocol::v2::{
        compact_block::ShortTxId, response::read_result_entry, txref::TransactionReference,
    };
    use zebra_chain::transaction::UnminedTxId;

    let coinbase_id = UnminedTxId::from(block_1.transactions[0].as_ref());
    let mut recv = send_request(
        &raw.connection,
        V2WireRequest::GetTx {
            refs: vec![
                TransactionReference::from(coinbase_id),
                TransactionReference::ShortId {
                    block_hash: block_1.hash(),
                    short_id: ShortTxId([0x0F; 6]),
                },
            ],
        },
    )
    .await;

    let found = tokio::time::timeout(
        TEST_TIMEOUT,
        read_result_entry::<zebra_chain::transaction::Transaction, _>(&mut recv, "get-tx"),
    )
    .await
    .expect("response arrives in time")
    .expect("response entry parses");
    match found {
        Some(tx) => assert_eq!(&tx, &block_1.transactions[0]),
        other => panic!("expected the sent block's coinbase, got: {other:?}"),
    }
    let missing =
        read_result_entry::<zebra_chain::transaction::Transaction, _>(&mut recv, "get-tx")
            .await
            .expect("response entry parses");
    assert!(
        missing.is_none(),
        "a SHORTID reference to a block sent without a compact block is not-found",
    );
}

/// `get-hashes` and `get-tree-roots` are served from the inbound service's
/// synchronization reads, a `get-tree-roots` request whose anchor is not in
/// the best chain is refused, and `get-block-range` streams the anchor's
/// ancestor chain in descending order with exact bounds.
#[tokio::test]
async fn v2_sync_primitives_are_served() {
    use zebra_chain::serialization::ZcashDeserialize;

    use crate::protocol::v2::{
        record,
        request::Request as V2WireRequest,
        response::{HashesResponse, TreeRootsResponse},
        types::ErrorCode,
    };

    let _init_guard = zebra_test::init();

    let raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    // get-hashes answers with the inbound service's entries.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetHashes {
            start_height: 0,
            stride: 400,
            count: 100,
        },
    )
    .await;
    let response = tokio::time::timeout(TEST_TIMEOUT, HashesResponse::read(&mut recv))
        .await
        .expect("response arrives in time")
        .expect("response parses");
    assert_eq!(response.0, test_sync_hash_entries(&raw.data));
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    // get-tree-roots answers with the inbound service's entries when the
    // anchor matches.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetTreeRoots {
            start_height: 0,
            final_hash: raw.data.block.hash(),
            count: 1,
        },
    )
    .await;
    let response = tokio::time::timeout(TEST_TIMEOUT, TreeRootsResponse::read(&mut recv))
        .await
        .expect("response arrives in time")
        .expect("response parses");
    assert_eq!(response.0, test_tree_roots_entries());
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    // A get-tree-roots request whose anchor is not in the best chain is
    // refused rather than answered with entries for different blocks.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetTreeRoots {
            start_height: 0,
            final_hash: zebra_chain::block::Hash([0xAB; 32]),
            count: 1,
        },
    )
    .await;
    assert_reset_with(
        &mut recv,
        ErrorCode::Refused,
        "an unknown anchor is refused",
    )
    .await;

    // get-block-range streams the anchor's ancestor chain in descending
    // order: block 2, then block 1, then the (unserved) genesis parent
    // truncates the stream.
    async fn read_block_range(
        recv: &mut quinn::RecvStream,
    ) -> (u8, Vec<Arc<zebra_chain::block::Block>>) {
        let result = record::read_u8(recv).await.expect("result byte parses");
        let mut blocks = Vec::new();
        if result != 0x00 {
            record::expect_end_of_stream(recv)
                .await
                .expect("nothing follows a not-found result");
            return (result, blocks);
        }
        loop {
            match record::read_record(recv).await.expect("record parses") {
                Some(payload) => blocks.push(Arc::new(
                    zebra_chain::block::Block::zcash_deserialize(payload.as_slice())
                        .expect("served block parses"),
                )),
                None => return (result, blocks),
            }
        }
    }

    // Each case is (name, anchor, count, max_bytes, expected result,
    // expected block hashes).
    let cases: [(&str, zebra_chain::block::Hash, u64, u64, u8, Vec<_>); 4] = [
        (
            "blocks stream in descending order from the anchor",
            raw.data.block_2.hash(),
            5,
            8_000_000,
            0x00,
            vec![raw.data.block_2.hash(), raw.data.block_1.hash()],
        ),
        (
            "the count bound is exact",
            raw.data.block_2.hash(),
            1,
            8_000_000,
            0x00,
            vec![raw.data.block_2.hash()],
        ),
        (
            "the first block is delivered regardless of max_bytes",
            raw.data.block_2.hash(),
            5,
            1,
            0x00,
            vec![raw.data.block_2.hash()],
        ),
        (
            "an unknown anchor answers not-found",
            zebra_chain::block::Hash([0xAB; 32]),
            5,
            8_000_000,
            0x02,
            vec![],
        ),
    ];

    for (name, final_hash, count, max_bytes, expected_result, expected_hashes) in cases {
        let mut recv = send_request(
            &connection,
            V2WireRequest::GetBlockRange {
                final_hash,
                count,
                max_bytes,
            },
        )
        .await;

        let (result, blocks) = tokio::time::timeout(TEST_TIMEOUT, read_block_range(&mut recv))
            .await
            .unwrap_or_else(|_| panic!("{name}: response arrives in time"));

        assert_eq!(result, expected_result, "{name}");
        let hashes: Vec<_> = blocks.iter().map(|block| block.hash()).collect();
        assert_eq!(hashes, expected_hashes, "{name}");
    }
}

/// `get-object` serves ranged reads of content-addressed artifacts from
/// the artifact directory: the object's size always answers, data never
/// extends past `length` bytes or the object's end, and unknown hashes
/// answer not-found.
#[tokio::test]
async fn v2_get_object_serves_ranged_artifact_reads() {
    use sha2::{Digest, Sha256};

    use crate::protocol::v2::{record, request::Request as V2WireRequest, types::ObjectHash};

    let _init_guard = zebra_test::init();

    // An artifact directory holding one 100,000-byte object named by the
    // hex hash of its contents.
    let temp = tempfile::tempdir().expect("temp dir creates");
    let artifact_dir = temp
        .path()
        .join("network")
        .join("artifacts")
        .join("mainnet");
    std::fs::create_dir_all(&artifact_dir).expect("artifact dir creates");
    let object: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
    let hash: [u8; 32] = Sha256::digest(&object).into();
    std::fs::write(artifact_dir.join(hex::encode(hash)), &object).expect("artifact writes");

    let config = Config {
        network: Network::Mainnet,
        cache_dir: crate::config::CacheDir::custom_path(temp.path()),
        ..Config::default()
    };
    let raw = raw_initiator_with_config(Arc::new(TestData::new()), config).await;
    let connection = raw.connection.clone();

    async fn get_object(
        connection: &quinn::Connection,
        request: V2WireRequest,
    ) -> (u8, u64, Vec<u8>) {
        let mut recv = send_request(connection, request).await;

        let result = record::read_u8(&mut recv)
            .await
            .expect("result byte parses");
        if result != 0x00 {
            record::expect_end_of_stream(&mut recv)
                .await
                .expect("nothing follows a not-found result");
            return (result, 0, Vec::new());
        }
        let size = record::read_compact_size(&mut recv)
            .await
            .expect("size parses");
        let mut data = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut recv, &mut data)
            .await
            .expect("data reads");
        (result, size, data)
    }

    // Each case is (name, hash, offset, length, expected result, expected data).
    let cases: [(&str, [u8; 32], u64, u64, u8, &[u8]); 5] = [
        ("the whole object", hash, 0, 200_000, 0x00, &object),
        (
            "a middle range delivers exactly the requested bytes",
            hash,
            40_000,
            10_000,
            0x00,
            &object[40_000..50_000],
        ),
        (
            "a range past the end is truncated at the object's size",
            hash,
            90_000,
            50_000,
            0x00,
            &object[90_000..],
        ),
        (
            "an offset at the size answers the size and no data",
            hash,
            100_000,
            1_000,
            0x00,
            &[],
        ),
        (
            "an unknown hash answers not-found",
            [0xEE; 32],
            0,
            16,
            0x02,
            &[],
        ),
    ];

    for (name, object_hash, offset, length, expected_result, expected_data) in cases {
        let (result, size, data) = tokio::time::timeout(
            TEST_TIMEOUT,
            get_object(
                &connection,
                V2WireRequest::GetObject {
                    hash: ObjectHash(object_hash),
                    offset,
                    length,
                },
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: response arrives in time"));

        assert_eq!(result, expected_result, "{name}");
        assert_eq!(data, expected_data, "{name}");

        // A found result always reports the object's full size, whatever
        // range of it was asked for.
        if result == 0x00 && offset + length <= object.len() as u64 {
            assert_eq!(size, object.len() as u64, "{name}");
        }
    }
}

/// `get-object` requests are refused while the artifact store does not
/// exist, and malformed sync requests are still connection errors: requests
/// are read and validated before refusal.
#[tokio::test]
async fn v2_get_object_is_refused_and_malformed_sync_requests_are_rejected() {
    use crate::protocol::v2::{
        request::Request as V2WireRequest,
        types::{ErrorCode, ObjectHash, StreamType},
    };

    let _init_guard = zebra_test::init();

    // The responder has no cache directory, so it has no artifact store.
    let config = Config {
        network: Network::Mainnet,
        cache_dir: crate::config::CacheDir::disabled(),
        ..Config::default()
    };
    let raw = raw_initiator_with_config(Arc::new(TestData::new()), config).await;
    let connection = raw.connection.clone();

    let request = V2WireRequest::GetObject {
        hash: ObjectHash([0x5A; 32]),
        offset: 0,
        length: 1_024,
    };
    let (mut send, mut recv) = connection.open_bi().await.expect("stream opens");
    let mut bytes = vec![request.stream_type().byte()];
    request.encode(&mut bytes).expect("request encodes");
    send.write_all(&bytes).await.expect("request writes");
    send.finish().expect("stream finishes");

    assert_reset_with(
        &mut recv,
        ErrorCode::Refused,
        "get-object requests are refused with REFUSED",
    )
    .await;

    // A malformed sync request (stride 0) is not refused but rejected:
    // requests are validated before the refusal, and a violation is a
    // connection error of type `PROTOCOL_ERROR`.
    let (mut send, _recv) = connection.open_bi().await.expect("stream opens");
    let mut bytes = vec![StreamType::GetHashes.byte()];
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.push(0x01);
    send.write_all(&bytes).await.expect("request writes");
    send.finish().expect("stream finishes");

    assert_closed_with(
        &connection,
        ErrorCode::ProtocolError,
        "a malformed sync request is a connection error",
    )
    .await;
}

/// Streams with unrecognized type bytes are refused with
/// `UNSUPPORTED_STREAM_TYPE` — bidirectional and unidirectional alike —
/// without a connection error or a misbehavior penalty, so future stream
/// types can be deployed without version gating.
#[tokio::test]
async fn v2_unknown_stream_types_are_refused_without_penalty() {
    use crate::protocol::v2::{
        record, request::Request as V2WireRequest, response::read_result_entry, types::ErrorCode,
    };

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    // An unrecognized bidirectional stream type: the first unassigned
    // request-range byte.
    let (mut send, mut recv) = connection.open_bi().await.expect("stream opens");
    send.write_all(&[0x0A]).await.expect("type byte writes");
    send.finish().expect("stream finishes");

    assert_reset_with(
        &mut recv,
        ErrorCode::UnsupportedStreamType,
        "unknown bidirectional stream types are refused",
    )
    .await;

    // An unrecognized unidirectional stream type: the first unassigned
    // announcement-range byte. Refusing a unidirectional stream involves
    // only cancelling the sender's direction.
    let mut send = connection.open_uni().await.expect("stream opens");
    send.write_all(&[0x13]).await.expect("type byte writes");
    let stopped = tokio::time::timeout(TEST_TIMEOUT, send.stopped())
        .await
        .expect("cancellation arrives in time")
        .expect("stream is stopped, not lost");
    assert_eq!(
        stopped.map(|code| ErrorCode::from_wire(code.into_inner())),
        Some(ErrorCode::UnsupportedStreamType),
        "unknown unidirectional stream types are refused",
    );

    // The connection survives, requests are still served, and no penalty
    // was assigned.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetBlocks {
            hashes: vec![raw.data.block.hash()],
        },
    )
    .await;

    let entry = tokio::time::timeout(
        TEST_TIMEOUT,
        read_result_entry::<zebra_chain::block::Block, _>(&mut recv, "get-blocks"),
    )
    .await
    .expect("response arrives in time")
    .expect("response entry parses");
    assert!(
        entry.is_some(),
        "the connection still serves requests after refused streams",
    );
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    assert!(
        raw.responder_misbehavior.try_recv().is_err(),
        "unknown stream types must not be penalized",
    );
}

/// A second concurrent announcement stream of the same type is a connection
/// error of type `PROTOCOL_ERROR`.
#[tokio::test]
async fn v2_second_announcement_stream_of_a_type_is_a_connection_error() {
    use crate::protocol::v2::types::{ErrorCode, StreamType};

    let _init_guard = zebra_test::init();

    let raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    // Open two concurrent transaction announcement streams, sending only
    // the type bytes: the second registration must fail the connection.
    let mut first = connection.open_uni().await.expect("stream opens");
    first
        .write_all(&[StreamType::TransactionAnnouncements.byte()])
        .await
        .expect("type byte writes");

    let mut second = connection.open_uni().await.expect("stream opens");
    second
        .write_all(&[StreamType::TransactionAnnouncements.byte()])
        .await
        .expect("type byte writes");

    assert_closed_with(
        &connection,
        ErrorCode::ProtocolError,
        "a second concurrent announcement stream of a type is a connection error",
    )
    .await;
}

/// After an announcement stream is reset, its sender may open a replacement
/// of the same type, and records on the replacement are processed.
#[tokio::test]
async fn v2_announcement_stream_reopens_after_reset() {
    use crate::protocol::v2::{
        record,
        txref::TransactionReference,
        types::{ErrorCode, StreamType},
    };

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    let announce = |txid_byte: u8| {
        let mut payload = Vec::new();
        TransactionReference::Txid(zebra_chain::transaction::Hash([txid_byte; 32]))
            .encode(&mut payload)
            .expect("write to Vec succeeds");
        let mut framed = vec![StreamType::TransactionAnnouncements.byte()];
        record::write_record(&mut framed, &payload).expect("write to Vec succeeds");
        framed
    };

    // Announce a transaction, and wait for it to reach the responder's
    // inbound service, so the reset cannot race the record.
    let mut first = connection.open_uni().await.expect("stream opens");
    first
        .write_all(&announce(0x01))
        .await
        .expect("record writes");

    let event = tokio::time::timeout(TEST_TIMEOUT, raw.responder_events.recv())
        .await
        .expect("announcement arrives in time")
        .expect("responder events channel is open");
    assert!(
        matches!(event, Request::AdvertiseTransactionIds(_, _)),
        "got: {event:?}",
    );

    // Reset the announcement stream, then open a replacement of the same
    // type: the replacement must be accepted and its records processed.
    first
        .reset(ErrorCode::InternalError.into())
        .expect("stream resets");

    // The responder deregisters the stream when it observes the reset, and
    // a replacement opened before that is a second concurrent stream, so
    // give the reset time to arrive: this wait makes the test reliable, and
    // a lost race fails it (the connection would close with a
    // `PROTOCOL_ERROR`).
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut second = connection.open_uni().await.expect("stream opens");
    second
        .write_all(&announce(0x02))
        .await
        .expect("record writes");

    let event = tokio::time::timeout(TEST_TIMEOUT, raw.responder_events.recv())
        .await
        .expect("replacement announcement arrives in time")
        .expect("responder events channel is open");
    assert!(
        matches!(event, Request::AdvertiseTransactionIds(_, _)),
        "got: {event:?}",
    );
}

/// A peer that opens a request stream and stalls without completing its
/// request is abandoned after `INBOUND_STREAM_TIMEOUT`: both stream
/// directions are cancelled with `CANCELLED`, the connection stays open,
/// and no penalty is assigned.
///
/// This test waits for a real inbound stream timeout, so it takes about
/// 40 seconds.
#[tokio::test]
async fn v2_stalled_inbound_request_stream_is_abandoned() {
    use crate::protocol::v2::{
        constants::INBOUND_STREAM_TIMEOUT,
        record,
        request::Request as V2WireRequest,
        response::read_result_entry,
        types::{ErrorCode, StreamType},
    };

    let _init_guard = zebra_test::init();

    let mut raw = raw_initiator(Arc::new(TestData::new())).await;
    let connection = raw.connection.clone();

    // A `get-tx` request that promises one reference and never sends it,
    // with the stream left open: the responder must not let the stalled
    // stream pin its task and buffers forever.
    let (mut send, mut recv) = connection.open_bi().await.expect("stream opens");
    send.write_all(&[StreamType::GetTx.byte(), 0x01])
        .await
        .expect("partial request writes");

    let stopped = tokio::time::timeout(INBOUND_STREAM_TIMEOUT + TEST_TIMEOUT, send.stopped())
        .await
        .expect("cancellation arrives in time")
        .expect("stream is stopped, not lost");
    assert_eq!(
        stopped.map(|code| ErrorCode::from_wire(code.into_inner())),
        Some(ErrorCode::Cancelled),
        "a stalled request stream is cancelled",
    );

    let mut buf = [0u8; 1];
    let result = tokio::time::timeout(TEST_TIMEOUT, recv.read(&mut buf))
        .await
        .expect("reset arrives in time");
    assert!(
        matches!(result, Err(quinn::ReadError::Reset(code))
            if ErrorCode::from_wire(code.into_inner()) == ErrorCode::Cancelled),
        "a stalled request stream's response direction is reset, got: {result:?}",
    );

    // The connection survives, requests are still served, and no penalty
    // was assigned: a stall is indistinguishable from a slow peer.
    let mut recv = send_request(
        &connection,
        V2WireRequest::GetBlocks {
            hashes: vec![raw.data.block.hash()],
        },
    )
    .await;

    let entry = tokio::time::timeout(
        TEST_TIMEOUT,
        read_result_entry::<zebra_chain::block::Block, _>(&mut recv, "get-blocks"),
    )
    .await
    .expect("response arrives in time")
    .expect("response entry parses");
    assert!(
        entry.is_some(),
        "the connection still serves requests after an abandoned stream",
    );
    record::expect_end_of_stream(&mut recv)
        .await
        .expect("response is complete");

    assert!(
        raw.responder_misbehavior.try_recv().is_err(),
        "a stalled stream must not be penalized",
    );
}
