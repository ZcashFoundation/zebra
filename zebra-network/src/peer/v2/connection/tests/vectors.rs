//! Loopback tests for the version 2 connection: two nodes connected over
//! QUIC, each presenting the other as a [`Client`].

use std::{collections::HashSet, net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use futures::{FutureExt, StreamExt};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinSet,
    time::Instant,
};
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
            addr_relay::{AddrRelay, AddrRelayHandle, Timings},
            connection::{
                announcements::AnnouncementWriter,
                global_object_permits,
                inbound::{
                    global_block_range_permits, global_hashes_permits,
                    serve as serve_request_stream, BlockRangeServing, ObjectServing, StreamPermits,
                    INBOUND_REQUEST_CONCURRENCY, MAX_LOOKUP_CALLS_PER_RESPONSE,
                    MAX_OBJECT_STREAMS_PER_PEER, RESPONSE_BYTES_LIMIT,
                },
                outbound::{
                    drive, ObjectSizes, RequestContext, RequestSend, ResponseGuard,
                    MAX_OUTBOUND_REQUEST_STREAMS,
                },
                spawn_connection, ConnectionParams, Event, Events, Overloads, EVENT_QUEUE,
                MAX_UNKNOWN_HANDSHAKE_RECORDS,
            },
            handshake::{CompletedHandshake, Role},
            misbehavior::MisbehaviorScores,
            nonces::NonceRegistry,
            tests::{connected_pair, connected_pair_with, identity, run_handshake},
        },
        Client, MustUseClientResponseSender,
    },
    peer_set::{ActiveConnectionCounter, InventoryChange},
    protocol::{
        external::{canonical_peer_addr, types::PeerServices, InventoryHash},
        internal::{InventoryResponse, Request, Response},
        v2::{
            announcement::{
                decode_addr_record, encode_addr_record, encode_tx_record, BLOCK_ANNOUNCEMENT_HEADER,
            },
            constants::MAX_RECORD_PAYLOAD_LEN,
            handshake::{InitRecord, INIT_RECORD_KIND},
            record::RecordCodec,
            request::{GetBlocks, GetHashes, GetHeaders, GetTx, Request as WireRequest},
            response::{
                decode_addr_response, encode_addr_response,
                hashes::{decode_hashes_response, MAX_HASHES_RESPONSE_LEN},
                read_headers, read_objects, write_headers_response, ObjectResult,
            },
            txref::{ShortTxId, TxRef},
            types::{ErrorCode, StreamType},
        },
    },
    quic::app_error_code,
    v2_address_book::{Message as SegmentMessage, V2BookHandle, Via},
    BanList, BoxError, MisbehaviorReport, PeerSocketAddr, MAX_TX_INV_IN_SENT_MESSAGE,
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
    /// The node's misbehavior scores, which another node can share.
    misbehavior: MisbehaviorScores,
    /// The peers the node's misbehavior scores reported for banning. A node
    /// sharing another node's scores reports on that node's receiver.
    misbehavior_rx: tokio::sync::mpsc::Receiver<MisbehaviorReport>,
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

/// The fixture as an inbound service, and the receiver its requests are
/// copied to.
fn fixture_service(
    fixture: Fixture,
) -> (TestService, tokio::sync::mpsc::UnboundedReceiver<Request>) {
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel();
    let inbound_service = service_fn(move |request: Request| {
        let fixture = fixture.clone();
        let inbound_tx = inbound_tx.clone();
        async move {
            let _ = inbound_tx.send(request.clone());
            Ok::<Response, BoxError>(fixture.answer(request))
        }
    });
    (BoxCloneService::new(inbound_service), inbound_rx)
}

/// Spawns a connection task over `connection` with the fixture as its
/// inbound service.
fn spawn_node(connection: Connection, completed: CompletedHandshake, fixture: Fixture) -> TestNode {
    let (inbound_service, inbound_rx) = fixture_service(fixture);
    spawn_node_with(connection, completed, inbound_service, inbound_rx)
}

/// Spawns a connection task over `connection` with `inbound_service`; its
/// requests are only seen on `inbound_rx` if the service sends them there.
fn spawn_node_with(
    connection: Connection,
    completed: CompletedHandshake,
    inbound_service: TestService,
    inbound_rx: tokio::sync::mpsc::UnboundedReceiver<Request>,
) -> TestNode {
    let (misbehavior_tx, misbehavior_rx) = tokio::sync::mpsc::channel(8);
    spawn_node_scored(
        connection,
        completed,
        inbound_service,
        inbound_rx,
        MisbehaviorScores::spawn(misbehavior_tx, 8),
        misbehavior_rx,
        Relaying::default(),
    )
}

/// A node's address relay hub, ban list, and version 2 address segment.
struct Relaying {
    addr_relay: Option<AddrRelayHandle>,
    bans: tokio::sync::watch::Receiver<BanList>,
    address_book: Option<V2BookHandle>,
}

impl Default for Relaying {
    /// No hub, no bans, and no address segment.
    fn default() -> Self {
        Relaying {
            addr_relay: None,
            bans: tokio::sync::watch::channel(BanList::default()).1,
            address_book: None,
        }
    }
}

impl Relaying {
    /// No hub and no bans, with `address_book` as the node's address segment.
    fn with_address_book(address_book: V2BookHandle) -> Self {
        Relaying {
            address_book: Some(address_book),
            ..Relaying::default()
        }
    }
}

/// Spawns a connection task over `connection` with `inbound_service` and the
/// `misbehavior` scores, whose reports arrive on `misbehavior_rx` if they
/// were spawned with its sender. With `relaying.address_book`, the connection
/// hands relayed addresses to it, serves `get-addr` from it, and asks the peer for
/// addresses if it dialled the peer.
fn spawn_node_scored(
    connection: Connection,
    completed: CompletedHandshake,
    inbound_service: TestService,
    inbound_rx: tokio::sync::mpsc::UnboundedReceiver<Request>,
    misbehavior: MisbehaviorScores,
    misbehavior_rx: tokio::sync::mpsc::Receiver<MisbehaviorReport>,
    relaying: Relaying,
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
            genesis_hash: Network::Mainnet.genesis_hash(),
            misbehavior: misbehavior.clone(),
            discovery: relaying.address_book.is_some(),
            address_book: relaying.address_book,
            bans: relaying.bans,
            addr_relay: relaying.addr_relay,
            serve_block_ranges: true,
            block_range_permits: global_block_range_permits(),
            hashes_permits: global_hashes_permits(),
            object_source: None,
            local_services: PeerServices::NODE_NETWORK,
            object_permits: global_object_permits(),
        },
        ObjectSizes::spawn(),
        None,
    )
    .0;
    TestNode {
        client,
        inv_rx,
        inbound_rx,
        misbehavior,
        misbehavior_rx,
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

/// Two connected nodes, the first dialled the second, each with a stand-in version 2 address
/// segment whose messages arrive on the returned receivers, and the address each node has for
/// the other.
async fn connected_nodes_with_segments(
    network: &Network,
) -> (
    (TestNode, mpsc::Receiver<SegmentMessage>, PeerSocketAddr),
    (TestNode, mpsc::Receiver<SegmentMessage>),
    (Endpoint, Endpoint),
) {
    let (outbound, inbound, client_ep, server_ep) = connected_pair(network).await;
    let (a, b) = (identity("/a/"), identity("/b/"));
    let (a_nonces, b_nonces) = (NonceRegistry::spawn(8), NonceRegistry::spawn(8));
    let (initiator, responder) = tokio::join!(
        run_handshake(&outbound, Role::Initiator, network, &a, &a_nonces),
        run_handshake(&inbound, Role::Responder, network, &b, &b_nonces),
    );
    let b_addr: PeerSocketAddr = canonical_peer_addr(server_ep.local_addr().expect("bound"));

    let node = |connection, completed, fixture| {
        let (inbound_service, inbound_rx) = fixture_service(fixture);
        let (misbehavior_tx, misbehavior_rx) = mpsc::channel(8);
        let (segment_tx, segment_rx) = mpsc::channel(8);
        let node = spawn_node_scored(
            connection,
            completed,
            inbound_service,
            inbound_rx,
            MisbehaviorScores::spawn(misbehavior_tx, 8),
            misbehavior_rx,
            Relaying::with_address_book(V2BookHandle::new(segment_tx)),
        );
        (node, segment_rx)
    };
    let fixture = Fixture::new();
    let (a, a_segment) = node(
        outbound,
        initiator.expect("the handshake completes"),
        fixture.clone(),
    );
    let (b, b_segment) = node(
        inbound,
        responder.expect("the handshake completes"),
        fixture,
    );
    (
        (a, a_segment, b_addr),
        (b, b_segment),
        (client_ep, server_ep),
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
    raw_peer_node_configured(network, inbound_service, relay, None).await
}

/// Connects a raw peer, which uses `client_config` if given, to a node and
/// completes the handshake by hand, with `relay` in the raw peer's `init`
/// record. The node serves `inbound_service`, or the fixture if `None`.
async fn raw_peer_node_configured(
    network: &Network,
    inbound_service: Option<TestService>,
    relay: bool,
    client_config: Option<quinn::ClientConfig>,
) -> RawPeer {
    raw_peer_node_scored(
        network,
        inbound_service,
        relay,
        client_config,
        None,
        Relaying::default(),
    )
    .await
}

/// Connects a raw peer to a node serving the fixture, which relays addresses
/// through `relaying`, and completes the handshake by hand. The raw peer
/// dials the node.
async fn raw_peer_node_with_hub(network: &Network, relaying: Relaying) -> RawPeer {
    raw_peer_node_scored(network, None, true, None, None, relaying).await
}

/// Connects a raw peer, which uses `client_config` if given, to a node and
/// completes the handshake by hand, with `relay` in the raw peer's `init`
/// record. The node serves `inbound_service`, or the fixture if `None`, and
/// scores the peer's misbehavior in `misbehavior`, or in scores of its own if
/// `None`. It relays addresses through `relaying`, and hands relayed addresses to its address
/// book, if it has one.
async fn raw_peer_node_scored(
    network: &Network,
    inbound_service: Option<TestService>,
    relay: bool,
    client_config: Option<quinn::ClientConfig>,
    misbehavior: Option<MisbehaviorScores>,
    relaying: Relaying,
) -> RawPeer {
    let (connection, inbound, client_ep, server_ep) =
        connected_pair_with(network, client_config).await;
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
    let (inbound_service, inbound_rx) = match inbound_service {
        Some(service) => (service, tokio::sync::mpsc::unbounded_channel().1),
        None => fixture_service(Fixture::new()),
    };
    let (misbehavior, misbehavior_rx) = match misbehavior {
        // The shared scores report on their own node's receiver; this one gets nothing.
        Some(misbehavior) => (misbehavior, tokio::sync::mpsc::channel(1).1),
        None => {
            let (misbehavior_tx, misbehavior_rx) = tokio::sync::mpsc::channel(8);
            (MisbehaviorScores::spawn(misbehavior_tx, 8), misbehavior_rx)
        }
    };
    let node = spawn_node_scored(
        inbound,
        completed,
        inbound_service,
        inbound_rx,
        misbehavior,
        misbehavior_rx,
        relaying,
    );

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

        // Peers: version 2 peers' addresses go to the version 2 address segment, so the legacy
        // request is answered at once, with nothing.
        let response = call(&mut a.client, Request::Peers).await.expect("peers");
        assert_eq!(response, Response::Peers(Vec::new()));

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

        // Block adverts need a header and are not sent yet: the peer set's advertiser skips the
        // closed advert channel, and the connection refuses an advert like a legacy one does.
        assert!(a.client.block_advert_tx.is_closed());
        let error = call(&mut a.client, Request::AdvertiseBlock(block::Hash([1; 32]), None))
            .await
            .expect_err("block adverts use the peer set's advertiser");
        assert!(
            error.inner_debug().contains("AdvertRequiresDedicatedService"),
            "{error:?}"
        );
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

        let mut recv = raw_request(&raw.connection, &WireRequest::Mempool).await;
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
///
/// The inbound service answers 64 IDs per lookup, so the request's few
/// hundred IDs fit in the response's lookup calls.
#[tokio::test]
async fn get_tx_answers_in_request_order_within_the_byte_budget() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let blocks = vector_blocks();
        let raw = raw_peer_node_with(&network, Some(subset_service(blocks.clone(), 64))).await;

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

        // get-tree-roots is a known request type this node does not serve: REFUSED, connection
        // open.
        let (mut send, mut recv) = raw.connection.open_bi().await.expect("opens");
        send.write_all(&[StreamType::GetTreeRoots.code()])
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

/// A `get-headers` response must be one the request could have selected
/// (draft, "`get-headers`"): it starts after a locator hash, or after the
/// genesis block for a peer that knows none of them; it ends at `hash_stop` if
/// it reaches it; and an empty locator selects only the header `hash_stop`
/// names. Any other response is misbehavior, for headers and hashes alike.
///
/// A response after the genesis block is used only if the locator includes
/// the genesis block; otherwise it is discarded, and the client gets no
/// headers.
#[tokio::test]
async fn header_responses_are_bound_to_the_request() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    /// What the client gets.
    #[derive(Debug)]
    enum Outcome {
        /// The raw peer's headers.
        Accepted,
        /// No headers.
        Discarded,
        /// A misbehavior error containing this text.
        Rejected(&'static str),
    }
    use Outcome::*;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let genesis = fixture.blocks[0].hash();
        let block_1 = fixture.blocks[1].clone();
        let block_2 = fixture.blocks[2].clone();
        let unknown = block::Hash([0xAB; 32]);

        // The locator, the stop hash, whether to ask for hashes rather than headers, the headers
        // the raw peer answers with, and what the client gets.
        let cases: [(
            Vec<block::Hash>,
            Option<block::Hash>,
            bool,
            Vec<&Arc<Block>>,
            Outcome,
        ); 15] = [
            (
                vec![genesis],
                None,
                false,
                vec![&block_1, &block_2],
                Accepted,
            ),
            (
                vec![block_1.hash(), genesis],
                None,
                false,
                vec![&block_2],
                Accepted,
            ),
            (
                vec![unknown, genesis],
                None,
                false,
                vec![&block_1, &block_2],
                Accepted,
            ),
            // A peer that knows none of the locator hashes answers after the genesis block, which
            // a locator without the genesis block does not connect to.
            (
                vec![unknown],
                None,
                false,
                vec![&block_1, &block_2],
                Discarded,
            ),
            (
                vec![unknown],
                None,
                true,
                vec![&block_1, &block_2],
                Discarded,
            ),
            // Being discarded does not excuse a response that is misbehavior.
            (
                vec![unknown],
                Some(block_1.hash()),
                true,
                vec![&block_1, &block_2],
                Rejected("past hash_stop"),
            ),
            (
                vec![genesis],
                None,
                false,
                vec![&block_2],
                Rejected("not a locator hash"),
            ),
            (
                vec![genesis],
                None,
                true,
                vec![&block_2],
                Rejected("not a locator hash"),
            ),
            (
                vec![genesis],
                Some(block_1.hash()),
                false,
                vec![&block_1],
                Accepted,
            ),
            (
                vec![genesis],
                Some(block_1.hash()),
                false,
                vec![&block_1, &block_2],
                Rejected("past hash_stop"),
            ),
            (
                vec![genesis],
                Some(block_1.hash()),
                true,
                vec![&block_1, &block_2],
                Rejected("past hash_stop"),
            ),
            (
                Vec::new(),
                Some(block_1.hash()),
                false,
                vec![&block_1],
                Accepted,
            ),
            (
                Vec::new(),
                Some(block_1.hash()),
                false,
                vec![&block_1, &block_2],
                Rejected("empty locator"),
            ),
            (
                Vec::new(),
                None,
                false,
                vec![&block_1],
                Rejected("empty locator"),
            ),
            (Vec::new(), None, false, Vec::new(), Accepted),
        ];

        for (case, (known_blocks, stop, hashes, reply, outcome)) in cases.into_iter().enumerate() {
            let mut raw = raw_peer_node(&network).await;
            let reply_headers: Vec<_> = reply.iter().map(|block| block.header.clone()).collect();
            let request = if hashes {
                Request::FindBlocks { known_blocks, stop }
            } else {
                Request::FindHeaders { known_blocks, stop }
            };

            let (result, ()) = tokio::join!(call(&mut raw.node.client, request), async {
                let (_, mut send, _recv) = accept_request(&raw.connection).await;
                let mut bytes = Vec::new();
                write_headers_response(&mut bytes, &reply_headers, false).expect("writes to a Vec");
                let _ = send.write_all(&bytes).await;
                let _ = send.finish();
            });

            let expected: Vec<_> = match outcome {
                Accepted => reply.iter().map(|block| block.hash()).collect(),
                Discarded => Vec::new(),
                Rejected(expected_error) => {
                    let inner = result
                        .expect_err("an unselectable response is rejected")
                        .inner_debug();
                    assert!(
                        inner.contains("ProtocolViolation") && inner.contains(expected_error),
                        "case {case}: expected {expected_error}, got {inner}"
                    );
                    continue;
                }
            };
            let response = result.unwrap_or_else(|error| {
                panic!("case {case}: a valid response is not an error: {error}")
            });
            let got: Vec<_> = match response {
                Response::BlockHeaders(headers) => headers
                    .iter()
                    .map(|counted| counted.header.hash())
                    .collect(),
                Response::BlockHashes(hashes) => hashes,
                response => panic!("case {case}: unexpected response {response:?}"),
            };
            assert_eq!(got, expected, "case {case}: {outcome:?}");
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A response after a genesis block that the locator does not include is
/// discarded without a penalty: as many of them as it takes misbehaving
/// responses to ban a peer leave the connection open and report nothing.
#[tokio::test]
async fn discarded_header_responses_are_not_penalized() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut after_genesis = Vec::new();
        write_headers_response(
            &mut after_genesis,
            &[
                fixture.blocks[1].header.clone(),
                fixture.blocks[2].header.clone(),
            ],
            false,
        )
        .expect("writes to a Vec");
        let mut raw = raw_peer_node(&network).await;

        let penalties_to_ban = crate::constants::MAX_PEER_MISBEHAVIOR_SCORE
            .div_ceil(crate::protocol::v2::constants::RESPONSE_MISBEHAVIOR_PENALTY);
        for index in 0..penalties_to_ban {
            let request = Request::FindBlocks {
                known_blocks: vec![block::Hash([0xAB; 32])],
                stop: None,
            };
            let (result, ()) = tokio::join!(call(&mut raw.node.client, request), async {
                let (_, mut send, _recv) = accept_request(&raw.connection).await;
                let _ = send.write_all(&after_genesis).await;
                let _ = send.finish();
            });
            let response =
                result.unwrap_or_else(|error| panic!("reply {index}: the client fails: {error}"));
            assert!(
                matches!(&response, Response::BlockHashes(hashes) if hashes.is_empty()),
                "reply {index}: the headers are discarded, got {response:?}"
            );
        }

        call(&mut raw.node.client, Request::Ping(Default::default()))
            .await
            .expect("the connection stays open");
        assert!(raw.connection.close_reason().is_none());
        assert!(
            raw.node.misbehavior_rx.try_recv().is_err(),
            "nothing is reported"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Misbehaving `get-headers` responses cost the peer: each one is discarded
/// with a penalty, the connection stays open below the ban threshold, a valid
/// response in between does not clear the score, and the response that
/// reaches the threshold closes the connection with `MISBEHAVIOR` and reports
/// the peer to the address book for banning (draft, "Misbehavior and
/// Banning").
#[tokio::test]
async fn misbehaving_responses_close_and_ban_at_the_threshold() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let genesis = fixture.blocks[0].hash();
        let block_1 = fixture.blocks[1].clone();
        let block_2 = fixture.blocks[2].clone();
        let mut raw = raw_peer_node(&network).await;

        let mut non_contiguous = Vec::new();
        write_headers_response(
            &mut non_contiguous,
            &[block_2.header.clone(), block_1.header.clone()],
            false,
        )
        .expect("writes to a Vec");
        // A count of 161 headers, one over the limit, is misbehavior from the count alone.
        let too_many = vec![161];
        let mut empty = Vec::new();
        write_headers_response(&mut empty, &[], false).expect("writes to a Vec");

        let penalties_to_ban = crate::constants::MAX_PEER_MISBEHAVIOR_SCORE
            .div_ceil(crate::protocol::v2::constants::RESPONSE_MISBEHAVIOR_PENALTY);
        let mut replies = vec![too_many, empty];
        let penalties_to_ban = usize::try_from(penalties_to_ban).expect("a small count fits");
        replies.extend(std::iter::repeat_n(non_contiguous, penalties_to_ban - 1));

        for (index, reply) in replies.iter().enumerate() {
            let is_last = index + 1 == replies.len();
            let request = Request::FindHeaders {
                known_blocks: vec![genesis],
                stop: None,
            };
            let (result, ()) = tokio::join!(call(&mut raw.node.client, request), async {
                let (_, mut send, _recv) = accept_request(&raw.connection).await;
                let _ = send.write_all(reply).await;
                let _ = send.finish();
            });

            if index == 1 {
                result.expect("a valid empty response is accepted");
            } else {
                let inner = result
                    .expect_err("a misbehaving response is rejected")
                    .inner_debug();
                assert!(
                    inner.contains("ProtocolViolation"),
                    "reply {index}: got {inner}"
                );
            }

            // The penalty is applied before the client gets its error, so the report, if any,
            // has already been sent.
            let report = raw.node.misbehavior_rx.try_recv();
            if is_last {
                let report = report.expect("the peer is reported for banning");
                assert_eq!(report.score, crate::constants::MAX_PEER_MISBEHAVIOR_SCORE);
                assert!(report.addr.ip().is_loopback(), "got {report:?}");
                assert!(
                    report.reason.contains("not a contiguous chain"),
                    "got {report:?}"
                );

                let close = raw.connection.closed().await;
                assert!(
                    matches!(&close, quinn::ConnectionError::ApplicationClosed(close)
                        if close.error_code.into_inner() == ErrorCode::Misbehavior.code()),
                    "got {close:?}"
                );
            } else {
                assert!(
                    report.is_err(),
                    "reply {index}: nothing is reported below the threshold"
                );
                call(&mut raw.node.client, Request::Ping(Default::default()))
                    .await
                    .unwrap_or_else(|error| panic!("reply {index}: the client fails: {error}"));
                assert!(
                    raw.connection.close_reason().is_none(),
                    "reply {index}: the connection stays open below the threshold"
                );
            }
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Has `raw` answer the node's next `FindHeaders` request with `reply`, a
/// response the node rejects as misbehavior.
async fn reply_with_misbehavior(raw: &mut RawPeer, reply: &[u8]) {
    let request = Request::FindHeaders {
        known_blocks: vec![Network::Mainnet.genesis_hash()],
        stop: None,
    };
    let (result, ()) = tokio::join!(call(&mut raw.node.client, request), async {
        let (_, mut send, _recv) = accept_request(&raw.connection).await;
        let _ = send.write_all(reply).await;
        let _ = send.finish();
    });
    let inner = result
        .expect_err("a misbehaving response is rejected")
        .inner_debug();
    assert!(inner.contains("ProtocolViolation"), "got {inner}");
}

/// A peer closed with `MISBEHAVIOR` keeps its full score until its ban is in
/// force: the address book applies ban reports at its next flush, and a peer
/// that reconnects before then, here a second raw peer from the same loopback
/// address scored by the same node, is closed with `MISBEHAVIOR` on its first
/// misbehaving response, without a second report.
#[tokio::test]
async fn reconnecting_banned_peers_are_closed_on_their_first_violation() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let mut non_contiguous = Vec::new();
        write_headers_response(
            &mut non_contiguous,
            &[
                fixture.blocks[2].header.clone(),
                fixture.blocks[1].header.clone(),
            ],
            false,
        )
        .expect("writes to a Vec");
        let penalties_to_ban = crate::constants::MAX_PEER_MISBEHAVIOR_SCORE
            .div_ceil(crate::protocol::v2::constants::RESPONSE_MISBEHAVIOR_PENALTY);

        let mut raw = raw_peer_node(&network).await;
        for _ in 0..penalties_to_ban {
            reply_with_misbehavior(&mut raw, &non_contiguous).await;
        }
        let close = raw.connection.closed().await;
        assert!(
            matches!(&close, quinn::ConnectionError::ApplicationClosed(close)
                if close.error_code.into_inner() == ErrorCode::Misbehavior.code()),
            "got {close:?}"
        );
        raw.node
            .misbehavior_rx
            .try_recv()
            .expect("the peer is reported for banning");

        // The report has not reached an address book: the peer reconnects, and the node scores
        // it as before.
        let mut again = raw_peer_node_scored(
            &network,
            None,
            true,
            None,
            Some(raw.node.misbehavior.clone()),
            Relaying::default(),
        )
        .await;
        reply_with_misbehavior(&mut again, &non_contiguous).await;
        let close = tokio::time::timeout(SHORT_WAIT * 4, again.connection.closed())
            .await
            .expect("the reconnected peer is closed on its first misbehaving response");
        assert!(
            matches!(&close, quinn::ConnectionError::ApplicationClosed(close)
                if close.error_code.into_inner() == ErrorCode::Misbehavior.code()),
            "got {close:?}"
        );
        assert!(
            raw.node.misbehavior_rx.try_recv().is_err(),
            "the peer is reported once"
        );
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
/// `ConnectionReceiveTimeout` and cancels its stream with `CANCELLED`;
/// requests from the peer that hold the node's request permits while the
/// inbound service never answers, and one that waits for a busy permit, are
/// all refused with `REFUSED` at their deadlines.
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
                .map(|recv| recv.read_to_end(1024))
                .collect::<Vec<_>>();
            // The requests holding the permits wait on the inbound service until their deadline,
            // and the extra one waits for a permit until its own, or until the others free their
            // permits a moment before it: every one is refused at its deadline.
            let mut codes = Vec::new();
            for read in futures::future::join_all(reads).await {
                let error = read.expect_err("the stream is reset");
                let quinn::ReadToEndError::Read(quinn::ReadError::Reset(code)) = error else {
                    panic!("expected a reset, got {error:?}");
                };
                codes.push(code.into_inner());
            }
            assert!(started.elapsed() >= REQUEST_TIMEOUT - Duration::from_secs(1));
            assert!(
                codes.iter().all(|code| *code == ErrorCode::Refused.code()),
                "every request is refused at its deadline: {codes:?}"
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

/// With discovery on, a node sends `get-addr` once, right after the handshake, and only on a
/// connection it dialled. The peer answers from its version 2 address segment, never from its
/// inbound service, and the whole answer goes to the dialler's segment, with the peer as its
/// source.
#[tokio::test]
async fn get_addr_is_sent_once_and_feeds_the_segment() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let ((mut a, mut a_segment, b_addr), (mut b, mut b_segment), _endpoints) =
            connected_nodes_with_segments(&network).await;
        let fixture = Fixture::new();

        // B, which A dialled, answers from its segment.
        match b_segment
            .recv()
            .await
            .expect("B's connection asks its segment")
        {
            SegmentMessage::GetAddrSample { reply } => reply
                .send(fixture.addrs.clone())
                .expect("B waits for the sample"),
            other => panic!("unexpected segment message {other:?}"),
        }

        // A hands the whole answer to its segment, as relayed by B.
        match a_segment
            .recv()
            .await
            .expect("A's connection hands over the answer")
        {
            SegmentMessage::Intake { source, addrs, via } => {
                assert_eq!(source, b_addr);
                assert_eq!(via, Via::GetAddr);
                assert_eq!(
                    addrs.iter().map(MetaAddr::addr).collect::<Vec<_>>(),
                    fixture.addrs.iter().map(MetaAddr::addr).collect::<Vec<_>>(),
                );
            }
            other => panic!("unexpected segment message {other:?}"),
        }

        // Nothing else: B, which accepted the connection, does not ask A, and A asks once.
        tokio::time::sleep(SHORT_WAIT).await;
        assert!(a_segment.try_recv().is_err(), "A's segment got one message");
        assert!(b_segment.try_recv().is_err(), "B's segment got one message");

        // The legacy address request never reaches a version 2 peer or an inbound service.
        for client in [&mut a.client, &mut b.client] {
            let response = call(client, Request::Peers).await.expect("peers");
            assert_eq!(response, Response::Peers(Vec::new()));
        }
        while let Ok(request) = a.inbound_rx.try_recv() {
            assert!(
                !matches!(request, Request::Peers),
                "A's inbound service was asked"
            );
        }
        while let Ok(request) = b.inbound_rx.try_recv() {
            assert!(
                !matches!(request, Request::Peers),
                "B's inbound service was asked"
            );
        }
        assert!(
            b_segment.try_recv().is_err(),
            "no second get-addr reached B"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// The peer set never routes the legacy address book's address request to a version 2 peer.
#[tokio::test]
async fn v2_clients_do_not_support_legacy_address_requests() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (a, _b, _e1, _e2) = connected_nodes(&network).await;
        let client = crate::peer::LoadTrackedClient::from(a.client);

        assert!(!client.supports(&Request::Peers));
        assert!(!client.supports(&Request::MempoolTransactionIds));
        assert!(client.supports(&Request::BlocksByHash(HashSet::new())));
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

        // From A to B: answered once, with nothing, since B has no version 2 address segment.
        let mut recv = raw_request(&a_connection, &WireRequest::Addr).await;
        let body = recv.read_to_end(1 << 20).await.expect("answered");
        let addrs = decode_addr_response(&body).expect("a well-formed response");
        assert!(addrs.is_empty());
        let mut recv = raw_request(&a_connection, &WireRequest::Addr).await;
        assert_reset(recv.read_to_end(1 << 20).await, ErrorCode::Refused);

        assert!(a_connection.close_reason().is_none());
        assert!(b_connection.close_reason().is_none());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// The peer's address announcements go to the version 2 address segment, with the peer as their
/// source, without the inbound service; its block announcements reach the inventory registry and
/// the inbound service.
#[tokio::test]
async fn inbound_announcements_reach_the_node() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (segment_tx, mut segment_rx) = mpsc::channel(8);
        let mut raw = raw_peer_node_scored(
            &network,
            None,
            true,
            None,
            None,
            Relaying::with_address_book(V2BookHandle::new(segment_tx)),
        )
        .await;
        let raw_addr: PeerSocketAddr =
            canonical_peer_addr(raw._endpoints.0.local_addr().expect("bound"));

        // A new connection's address budget is one record.
        let addrs: Vec<MetaAddr> = ["203.0.113.7:8233"]
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

        // The announcements reach the segment once their reader has handed them over, in one
        // or more batches.
        let mut received = Vec::new();
        while received.len() < addrs.len() {
            match segment_rx.recv().await.expect("the node hands them over") {
                SegmentMessage::Intake { source, addrs, via } => {
                    assert_eq!(source, raw_addr);
                    assert_eq!(via, Via::Announcement);
                    received.extend(addrs);
                }
                other => panic!("unexpected segment message {other:?}"),
            }
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

/// An inbound service whose `get-mempool` snapshots are distinct witnessed
/// transaction IDs, the largest `get-mempool` records: `counts[n]` of them
/// in its `n`th snapshot, and the last count after that.
fn witnessed_mempool_service(counts: Vec<usize>) -> TestService {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    BoxCloneService::new(service_fn(move |request: Request| {
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let count = counts.get(call).or(counts.last()).copied().unwrap_or(0);
        async move {
            Ok::<Response, BoxError>(match request {
                Request::MempoolTransactionIds => Response::TransactionIds(
                    (0..count)
                        .map(|index| {
                            let mut hash = [0; 32];
                            // The index fits in the hash's first 8 bytes.
                            hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
                            UnminedTxId::Witnessed(transaction::WtxId {
                                id: transaction::Hash(hash),
                                auth_digest: transaction::AuthDigest([0x5A; 32]),
                            })
                        })
                        .collect(),
                ),
                _ => Response::Nil,
            })
        }
    }))
}

/// A response is bounded by one deadline from when its stream is accepted,
/// however the peer paces its reads: a peer that reads just fast enough to
/// keep every write moving still has the stream reset at `REQUEST_TIMEOUT`,
/// with `CANCELLED`, since the response had begun, and the connection stays
/// open.
///
/// The raw peer grants the node a small receive window and reads one
/// `get-mempool` snapshot, about 1.7 MB, at about 32 kB a second, so it
/// releases flow-control credit many times a second, and no write waits
/// long, but the snapshot would take almost a minute.
///
/// This takes `REQUEST_TIMEOUT` of real time: pausing tokio's clock is
/// unsafe with real sockets.
#[tokio::test]
async fn slowly_read_responses_end_at_the_request_deadline() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let max_ids: usize = MAX_TX_INV_IN_SENT_MESSAGE.try_into().expect("fits");
        let window = 64 * 1024;
        let client_config =
            crate::quic::client_config_with_receive_windows(&network, window, 2 * window);
        let raw = raw_peer_node_configured(
            &network,
            Some(witnessed_mempool_service(vec![max_ids])),
            true,
            Some(client_config),
        )
        .await;

        let started = Instant::now();
        let mut slow = raw_request(&raw.connection, &WireRequest::Mempool).await;
        let mut buffer = [0u8; 3200];
        let mut received = 0;
        let read = loop {
            match slow.read(&mut buffer).await {
                Ok(Some(len)) => received += len,
                other => break other,
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        let elapsed = started.elapsed();
        assert!(
            elapsed >= REQUEST_TIMEOUT - Duration::from_secs(1)
                && elapsed <= REQUEST_TIMEOUT + Duration::from_secs(5),
            "the stream ends at its deadline, after {elapsed:?}"
        );
        assert!(
            received > 4 * usize::try_from(window).expect("fits"),
            "the response kept moving until its deadline: {received} bytes"
        );
        match read {
            Err(quinn::ReadError::Reset(code)) => {
                assert_eq!(code, app_error_code(ErrorCode::Cancelled))
            }
            other => panic!("expected a reset, got {other:?}"),
        }
        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An inbound service call is bounded by the request deadline too: a
/// request the inbound service never answers has its stream reset with
/// `REFUSED` at `REQUEST_TIMEOUT`, since none of the response was written,
/// and the connection stays open.
///
/// This takes `REQUEST_TIMEOUT` of real time.
#[tokio::test]
async fn unanswered_lookups_end_at_the_request_deadline() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let hanging = service_fn(|_request: Request| futures::future::pending());
        let raw = raw_peer_node_with(&network, Some(BoxCloneService::new(hanging))).await;
        let started = Instant::now();

        let request = WireRequest::Tx(GetTx {
            txrefs: vec![TxRef::Txid(transaction::Hash([0xCD; 32]))],
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        assert_reset(recv.read_to_end(1024).await, ErrorCode::Refused);
        assert!(started.elapsed() >= REQUEST_TIMEOUT - Duration::from_secs(1));
        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A response the deadline interrupts part way is cancelled, not refused: a
/// `get-blocks` request whose second lookup the inbound service never
/// answers delivers its first entry, then has its stream reset with
/// `CANCELLED` at `REQUEST_TIMEOUT`, so the requester keeps the entry it
/// received instead of learning that its request was refused, and the
/// connection stays open.
///
/// This takes `REQUEST_TIMEOUT` of real time.
#[tokio::test]
async fn interrupted_responses_are_cancelled_at_the_request_deadline() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let block = Fixture::new().blocks[1].clone();
        // Answers a lookup of `block`'s hash with that block alone, and never answers another.
        let service = {
            let block = block.clone();
            service_fn(move |request: Request| {
                let block = block.clone();
                async move {
                    match request {
                        Request::BlocksByHash(hashes) if hashes.contains(&block.hash()) => {
                            Ok::<Response, BoxError>(Response::Blocks(vec![
                                InventoryResponse::Available((block, None)),
                            ]))
                        }
                        _ => futures::future::pending().await,
                    }
                }
            })
        };
        let raw = raw_peer_node_with(&network, Some(BoxCloneService::new(service))).await;
        let started = Instant::now();

        let request = WireRequest::Blocks(GetBlocks {
            hashes: vec![block.hash(), block::Hash([0xAB; 32])],
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let mut received = Vec::new();
        let read = loop {
            match recv.read_chunk(usize::MAX, true).await {
                Ok(Some(chunk)) => received.extend_from_slice(&chunk.bytes),
                other => break other,
            }
        };
        assert!(started.elapsed() >= REQUEST_TIMEOUT - Duration::from_secs(1));
        assert_eq!(received, found(&block), "the first entry is delivered");
        match read {
            Err(quinn::ReadError::Reset(code)) => {
                assert_eq!(code, app_error_code(ErrorCode::Cancelled))
            }
            other => panic!("expected a reset, got {other:?}"),
        }
        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An inbound service that answers one call from the fixture, is busy until
/// `shutdown` resolves, and has shut down after that: it fails while a
/// response is under way, at a moment the test chooses.
#[derive(Clone)]
struct ShutsDownAfterOneCall {
    fixture: Fixture,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    shutdown: futures::future::Shared<tokio::sync::oneshot::Receiver<()>>,
}

impl Service<Request> for ShutsDownAfterOneCall {
    type Response = Response;
    type Error = BoxError;
    type Future = futures::future::Ready<Result<Response, BoxError>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), BoxError>> {
        if self.calls.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        std::future::Future::poll(std::pin::Pin::new(&mut self.shutdown), cx)
            .map(|_shutdown| Err("the inbound service has shut down".into()))
    }

    fn call(&mut self, request: Request) -> Self::Future {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        futures::future::ready(Ok(self.fixture.answer(request)))
    }
}

/// A response abandoned because the inbound service shut down part way
/// through it is cancelled, not refused, like one abandoned at its deadline,
/// and the connection task is told to close the connection. The stream task
/// is driven directly, with nothing closing the connection, so the reset
/// reaches the requester.
#[tokio::test]
async fn interrupted_responses_are_cancelled_when_the_inbound_service_shuts_down() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let fixture = Fixture::new();
        let block = fixture.blocks[1].clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let service = ShutsDownAfterOneCall {
            fixture,
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown: shutdown_rx.shared(),
        };
        let (events, mut fatal_rx, mut events_rx) = Events::channel();

        // The first hash is looked up and written; the second lookup waits on the busy service.
        let request = WireRequest::Blocks(GetBlocks {
            hashes: vec![block.hash(), block::Hash([0xAB; 32])],
        });
        let mut recv = raw_request(&requester, &request).await;
        let (send, server_recv) = responder.accept_bi().await.expect("accepts");
        let served = tokio::spawn(serve_request_stream(
            send,
            server_recv,
            service,
            StreamPermits::new(global_hashes_permits()),
            Arc::new(Overloads::new()),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            None,
            events,
            BlockRangeServing::new(true, global_block_range_permits()),
            ObjectServing {
                source: None,
                advertised: false,
                peer_permits: Arc::new(tokio::sync::Semaphore::new(MAX_OBJECT_STREAMS_PER_PEER)),
                global_permits: global_object_permits(),
                stall_backoff: Arc::new(
                    crate::peer::v2::connection::inbound::hashes::StallBackoff::new(),
                ),
            },
        ));

        let expected = found(&block);
        let mut received = vec![0; expected.len()];
        recv.read_exact(&mut received)
            .await
            .expect("the first entry is delivered");
        assert_eq!(received, expected);
        assert!(
            fatal_rx.try_recv().is_err(),
            "nothing closes the connection while the service is busy"
        );

        // The inbound service shuts down between two lookups: the response is abandoned part way.
        shutdown_tx.send(()).expect("the service is waiting");
        assert_reset(recv.read_to_end(1024).await, ErrorCode::Cancelled);
        served.await.expect("the stream task completes");
        assert!(
            matches!(fatal_rx.try_recv(), Ok(Event::ServiceShutdown)),
            "the connection task is told to close the connection"
        );
        assert!(events_rx.try_recv().is_err(), "no other event is queued");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An inbound service that reports every block and transaction missing, as
/// the inbound service does for inventory it does not have, and counts its
/// lookup calls.
fn all_missing_service(calls: Arc<std::sync::atomic::AtomicUsize>) -> TestService {
    BoxCloneService::new(service_fn(move |request: Request| {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            Ok::<Response, BoxError>(match request {
                Request::BlocksByHash(hashes) => {
                    Response::Blocks(hashes.into_iter().map(InventoryResponse::Missing).collect())
                }
                Request::TransactionsById(ids) => Response::Transactions(
                    ids.into_iter().map(InventoryResponse::Missing).collect(),
                ),
                _ => Response::Nil,
            })
        }
    }))
}

/// One `get-blocks` or `get-tx` request makes a few batched inbound service
/// calls, however many entries it asks for: a maximum request of missing
/// blocks takes 12 calls, as its lookups grow from one hash to 16 a call, a
/// maximum request of missing transactions takes 7 calls for the 25,000 IDs
/// it looks up, and an inbound service that answers one key per call is
/// asked at most `MAX_LOOKUP_CALLS_PER_RESPONSE` times, after which the
/// response finishes early with the entries already written.
#[tokio::test]
async fn batch_requests_make_few_lookup_calls() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let raw = raw_peer_node_with(&network, Some(all_missing_service(calls.clone()))).await;
        let take_calls = || calls.swap(0, std::sync::atomic::Ordering::SeqCst);

        let max_blocks = crate::protocol::v2::constants::MAX_BLOCKS_PER_REQUEST;
        let hashes: Vec<_> = (0..max_blocks)
            .map(|index| {
                let mut hash = [0xEE; 32];
                // The index of a 128-hash request fits in 8 bytes.
                hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
                block::Hash(hash)
            })
            .collect();
        let request = WireRequest::Blocks(GetBlocks { hashes });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, max_blocks)
            .await
            .expect("a well-formed response");
        assert_eq!(entries.len(), max_blocks, "every hash is answered");
        assert!(entries
            .iter()
            .all(|entry| matches!(entry, ObjectResult::NotFound)));
        assert_eq!(take_calls(), 12);

        let max_txs = crate::protocol::v2::constants::MAX_TXS_PER_REQUEST;
        let txrefs: Vec<_> = (0..max_txs)
            .map(|index| {
                let mut hash = [0xCD; 32];
                // The index of a 50,000-reference request fits in 8 bytes.
                hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
                TxRef::Txid(transaction::Hash(hash))
            })
            .collect();
        let request = WireRequest::Tx(GetTx { txrefs });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Transaction>(&mut recv, max_txs)
            .await
            .expect("a well-formed response that finishes early");
        let looked_up: usize = MAX_TX_INV_IN_SENT_MESSAGE.try_into().expect("fits");
        assert_eq!(entries.len(), looked_up, "the looked up IDs are answered");
        assert!(entries
            .iter()
            .all(|entry| matches!(entry, ObjectResult::NotFound)));
        assert_eq!(take_calls(), 7);

        // One block per call: the response stops at the call limit.
        let blocks = vector_blocks();
        let raw = raw_peer_node_with(&network, Some(subset_service(blocks.clone(), 1))).await;
        let small: Vec<_> = blocks
            .iter()
            .filter(|block| entry_len(block) < 20_000)
            .map(|block| block.hash())
            .collect();
        assert!(small.len() > MAX_LOOKUP_CALLS_PER_RESPONSE);
        let request = WireRequest::Blocks(GetBlocks {
            hashes: small.clone(),
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, small.len())
            .await
            .expect("a well-formed response that finishes early");
        assert!(!entries.is_empty() && entries.len() <= MAX_LOOKUP_CALLS_PER_RESPONSE);
        for (entry, hash) in entries.iter().zip(&small) {
            assert!(matches!(entry, ObjectResult::Found(block) if block.hash() == *hash));
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An inbound service that knows `blocks`, reports every other hash
/// missing, and records how many hashes each block lookup asks for on
/// `batch_sizes`.
fn recording_block_service(
    blocks: Vec<Arc<Block>>,
    batch_sizes: tokio::sync::mpsc::UnboundedSender<usize>,
) -> TestService {
    let blocks = Arc::new(blocks);
    BoxCloneService::new(service_fn(move |request: Request| {
        let blocks = blocks.clone();
        let batch_sizes = batch_sizes.clone();
        async move {
            Ok::<Response, BoxError>(match request {
                Request::BlocksByHash(hashes) => {
                    let _ = batch_sizes.send(hashes.len());
                    Response::Blocks(
                        hashes
                            .into_iter()
                            .map(|hash| match blocks.iter().find(|b| b.hash() == hash) {
                                Some(block) => InventoryResponse::Available((block.clone(), None)),
                                None => InventoryResponse::Missing(hash),
                            })
                            .collect(),
                    )
                }
                _ => Response::Nil,
            })
        }
    }))
}

/// A `get-blocks` response looks up one hash in its first inbound service
/// call, and doubles the lookup after each call the inbound service answers
/// in full, up to 16 hashes: a maximum request of missing hashes takes 12
/// calls, and a request whose first block fills the byte budget has that
/// block alone looked up, not 16 blocks for the one it is sent.
#[tokio::test]
async fn block_lookups_grow_from_one_hash() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let blocks = vector_blocks();
        let (batch_sizes_tx, mut batch_sizes_rx) = tokio::sync::mpsc::unbounded_channel();
        let service = recording_block_service(blocks.clone(), batch_sizes_tx);
        let raw = raw_peer_node_with(&network, Some(service)).await;
        // The lookups are all recorded before the response finishes.
        let mut take_batch_sizes = || {
            let mut sizes = Vec::new();
            while let Ok(size) = batch_sizes_rx.try_recv() {
                sizes.push(size);
            }
            sizes
        };

        let max_blocks = crate::protocol::v2::constants::MAX_BLOCKS_PER_REQUEST;
        let made_up: Vec<_> = (0..max_blocks)
            .map(|index| {
                let mut hash = [0xEE; 32];
                // The index of a 128-hash request fits in 8 bytes.
                hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
                block::Hash(hash)
            })
            .collect();
        let request = WireRequest::Blocks(GetBlocks {
            hashes: made_up.clone(),
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, max_blocks)
            .await
            .expect("a well-formed response");
        assert_eq!(entries.len(), max_blocks, "every hash is answered");
        assert_eq!(
            take_batch_sizes(),
            [1, 2, 4, 8, 16, 16, 16, 16, 16, 16, 16, 1],
            "the lookups double up to 16 hashes a call"
        );

        // A block larger than the budget first: the response is that block alone, and so is
        // the lookup.
        let large = blocks
            .iter()
            .max_by_key(|block| entry_len(block))
            .expect("vector blocks")
            .clone();
        assert!(entry_len(&large) > RESPONSE_BYTES_LIMIT);
        let mut hashes = made_up;
        hashes[0] = large.hash();
        let request = WireRequest::Blocks(GetBlocks { hashes });
        let mut recv = raw_request(&raw.connection, &request).await;
        let entries = read_objects::<_, Block>(&mut recv, max_blocks)
            .await
            .expect("a well-formed response that finishes early");
        assert!(
            matches!(&entries[..], [ObjectResult::Found(found)] if found.hash() == large.hash())
        );
        assert_eq!(
            take_batch_sizes(),
            [1],
            "only the block that is sent is looked up"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An inbound service that fails every call with `error`, like the inbound
/// service's load-shed or timeout layer.
fn failing_service(error: fn() -> BoxError) -> TestService {
    BoxCloneService::new(service_fn(move |_request: Request| async move {
        Err::<Response, BoxError>(error())
    }))
}

/// Waits for the node to close `raw`'s connection, and checks it closed
/// gracefully, with `NO_ERROR`, and that the client's error is
/// `expected_error`.
async fn assert_overload_close(raw: &mut RawPeer, expected_error: &str) {
    let close = tokio::time::timeout(SHORT_WAIT * 10, raw.connection.closed())
        .await
        .expect("the overloaded connection is closed");
    match close {
        quinn::ConnectionError::ApplicationClosed(close) => {
            assert_eq!(close.error_code.into_inner(), ErrorCode::NoError.code());
        }
        other => panic!("unexpected close {other:?}"),
    }
    let error = call(&mut raw.node.client, Request::Ping(Default::default()))
        .await
        .expect_err("the connection failed");
    assert!(
        error.inner_debug().contains(expected_error),
        "expected {expected_error}, got {}",
        error.inner_debug()
    );
}

/// A peer whose requests keep overloading the inbound service is
/// disconnected, as on the legacy connection: each overload refuses its
/// stream and may close the connection, more likely the more recent the
/// last overload, so a peer that retries at once is soon closed with
/// `NO_ERROR`, and the client's error is `Overloaded`.
#[tokio::test]
async fn overloaded_requests_close_the_connection() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let service = failing_service(|| tower::load_shed::error::Overloaded::new().into());
        let mut raw = raw_peer_node_with(&network, Some(service)).await;
        let request = WireRequest::Blocks(GetBlocks {
            hashes: vec![block::Hash([0xAB; 32])],
        })
        .encode()
        .expect("encodes");

        // Each retry after an overload a moment ago is dropped with probability near one half,
        // so 100 retries leave the connection open with negligible probability.
        let mut refused = 0;
        for _ in 0..100 {
            let Ok((mut send, mut recv)) = raw.connection.open_bi().await else {
                break;
            };
            let sent = async {
                send.write_all(&[StreamType::GetBlocks.code()]).await?;
                send.write_all(&request).await?;
                send.finish()?;
                Ok::<(), BoxError>(())
            };
            if sent.await.is_err() {
                break;
            }
            match recv.read_to_end(1024).await {
                Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(code)))
                    if code == app_error_code(ErrorCode::Refused) =>
                {
                    refused += 1
                }
                _ => break,
            }
        }
        assert!(refused > 0, "overloaded requests are refused");
        assert_overload_close(&mut raw, "Overloaded").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Announcements that time out in the inbound service count as overloads
/// too: a peer that keeps announcing is soon closed with `NO_ERROR`, and
/// the client's error is `InboundTimeout`.
#[tokio::test]
async fn timed_out_announcements_close_the_connection() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let service = failing_service(|| tower::timeout::error::Elapsed::new().into());
        let mut raw = raw_peer_node_with(&network, Some(service)).await;

        let mut send = raw.connection.open_uni().await.expect("opens");
        send.write_all(&[StreamType::TransactionAnnouncements.code()])
            .await
            .expect("writes");
        // One announcement at a time, so each is its own inbound service call.
        for index in 0..100u8 {
            let id = UnminedTxId::Legacy(transaction::Hash([index; 32]));
            let record = RecordCodec::encode_record(encode_tx_record(id)).expect("valid");
            if send.write_all(&record).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            if raw.connection.close_reason().is_some() {
                break;
            }
        }
        assert_overload_close(&mut raw, "InboundTimeout").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An inbound service that has shut down: it never becomes ready.
#[derive(Clone)]
struct ShutDownService;

impl Service<Request> for ShutDownService {
    type Response = Response;
    type Error = BoxError;
    type Future = futures::future::Ready<Result<Response, BoxError>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), BoxError>> {
        std::task::Poll::Ready(Err("the inbound service has shut down".into()))
    }

    fn call(&mut self, _request: Request) -> Self::Future {
        futures::future::ready(Err("called while not ready".into()))
    }
}

/// A request or announcement that finds the inbound service shut down
/// closes the connection with `NO_ERROR`, and the client's error is
/// `ServiceShutdown`, as on the legacy connection.
#[tokio::test]
async fn inbound_service_shutdown_closes_the_connection() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let shut_down = || Some(BoxCloneService::new(ShutDownService));

        let mut raw = raw_peer_node_with(&network, shut_down()).await;
        let request = WireRequest::Blocks(GetBlocks {
            hashes: vec![block::Hash([0xAB; 32])],
        });
        let mut recv = raw_request(&raw.connection, &request).await;
        assert!(recv.read_to_end(1024).await.is_err(), "no response is sent");
        assert_overload_close(&mut raw, "ServiceShutdown").await;

        let mut raw = raw_peer_node_with(&network, shut_down()).await;
        let mut send = raw.connection.open_uni().await.expect("opens");
        send.write_all(&[StreamType::TransactionAnnouncements.code()])
            .await
            .expect("writes");
        let id = UnminedTxId::Legacy(transaction::Hash([0xAB; 32]));
        let record = RecordCodec::encode_record(encode_tx_record(id)).expect("valid");
        send.write_all(&record).await.expect("writes");
        assert_overload_close(&mut raw, "ServiceShutdown").await;
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Handshake-stream records of unknown kinds are ignored, as the draft
/// requires, and never close the connection, but only a few are read a
/// minute: the node leaves the rest unread, so a peer that keeps sending
/// them is stopped by its flow control, and the connection carries on.
#[tokio::test]
async fn unknown_handshake_records_beyond_the_rate_are_left_unread() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = raw_peer_node(&network).await;
        let unknown = RecordCodec::encode_record([0x07, 0xDE, 0xAD]).expect("valid");
        for _ in 0..MAX_UNKNOWN_HANDSHAKE_RECORDS {
            raw.hs_send.write_all(&unknown).await.expect("writes");
        }
        call(&mut raw.node.client, Request::Ping(Default::default()))
            .await
            .expect("the connection is open");

        // More unknown records than the stream's 4 MiB receive window: none are read, so the
        // writes stall on flow control.
        let mut large = vec![0x07];
        large.resize(MAX_RECORD_PAYLOAD_LEN, 0xAB);
        let large = RecordCodec::encode_record(&large).expect("valid");
        let flood = async {
            for _ in 0..3 {
                raw.hs_send.write_all(&large).await.expect("writes");
            }
        };
        assert!(
            tokio::time::timeout(SHORT_WAIT * 4, flood).await.is_err(),
            "the node stops reading the handshake stream"
        );

        call(&mut raw.node.client, Request::Ping(Default::default()))
            .await
            .expect("the connection is still open");
        assert!(raw.connection.close_reason().is_none());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A stream task's violation closes the connection at once, with
/// `PROTOCOL_ERROR`, however the peer keeps the connection task busy
/// beforehand: with address announcements beyond the event queue, and with
/// handshake records of unknown kinds, below and beyond the rate the node
/// reads them at.
#[tokio::test]
async fn violations_close_the_connection_behind_address_and_handshake_floods() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut raw = raw_peer_node(&network).await;

        let addr = MetaAddr::new_gossiped_meta_addr(
            "203.0.113.7:8233".parse::<SocketAddr>().unwrap().into(),
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        );
        // A one-address `get-addr` response is a one-byte count, then the record.
        let response = encode_addr_response(std::slice::from_ref(&addr)).expect("encodes");
        let addr_record = RecordCodec::encode_record(&response[1..]).expect("valid");
        let mut addr_send = raw.connection.open_uni().await.expect("opens");
        addr_send
            .write_all(&[StreamType::AddressAnnouncements.code()])
            .await
            .expect("writes");
        addr_send
            .write_all(&addr_record.repeat(8 * EVENT_QUEUE))
            .await
            .expect("writes");

        let unknown = RecordCodec::encode_record([0x07, 0xDE, 0xAD]).expect("valid");
        raw.hs_send
            .write_all(&unknown.repeat(4 * MAX_UNKNOWN_HANDSHAKE_RECORDS as usize))
            .await
            .expect("writes");

        // A transaction announcement whose reference type byte is undefined.
        let mut send = raw.connection.open_uni().await.expect("opens");
        send.write_all(&[StreamType::TransactionAnnouncements.code()])
            .await
            .expect("writes");
        let record = RecordCodec::encode_record([0xFF]).expect("valid");
        send.write_all(&record).await.expect("writes");

        let close = tokio::time::timeout(SHORT_WAIT * 4, raw.connection.closed())
            .await
            .expect("the violation closes the connection promptly");
        match close {
            quinn::ConnectionError::ApplicationClosed(close) => {
                assert_eq!(
                    close.error_code.into_inner(),
                    ErrorCode::ProtocolError.code()
                );
            }
            other => panic!("unexpected close {other:?}"),
        }
        let error = call(&mut raw.node.client, Request::Ping(Default::default()))
            .await
            .expect_err("the connection failed");
        assert!(
            error.inner_debug().contains("ProtocolViolation"),
            "got {}",
            error.inner_debug()
        );
        drop((addr_send, send));
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A stream task's connection-closing event reaches the connection task
/// however many best-effort events are waiting, and handing over either
/// kind never waits: address events beyond the queue are dropped.
#[tokio::test]
async fn connection_closing_events_are_never_crowded_out() {
    let _init_guard = zebra_test::init();

    let (events, mut fatal_rx, mut events_rx) = Events::channel();
    let addrs = Fixture::new().addrs;
    tokio::time::timeout(SHORT_WAIT, async {
        for _ in 0..2 * EVENT_QUEUE {
            events.addresses(addrs.clone());
        }
    })
    .await
    .expect("best-effort events never wait");

    events.close(Event::Violation {
        reason: "nonconforming record".to_string(),
        code: ErrorCode::ProtocolError,
    });
    assert!(
        matches!(
            fatal_rx.try_recv(),
            Ok(Event::Violation {
                code: ErrorCode::ProtocolError,
                ..
            })
        ),
        "the violation is queued for the connection task"
    );

    let mut queued = 0;
    while let Ok(event) = events_rx.try_recv() {
        assert!(matches!(event, Event::Addresses(_)));
        queued += 1;
    }
    assert_eq!(
        queued, EVENT_QUEUE,
        "address events beyond the queue are dropped"
    );
}

/// A request task aborted before it first runs still answers its client:
/// the connection task wraps the sender in its guard before spawning, so
/// dropping the unstarted task reports the connection as dropped instead of
/// dropping an unanswered sender, which panics.
#[tokio::test]
async fn request_task_aborted_before_it_runs_answers_the_client() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (node_connection, _raw_connection, _e1, _e2) = connected_pair(&network).await;
        let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(8);
        let (events, _fatal_rx, _events_rx) = Events::channel();
        let request = Request::Peers;

        let (tx, rx) = futures::channel::oneshot::channel();
        let tx = MustUseClientResponseSender::new(tx, &request, None, None);
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = permits.clone().try_acquire_owned().expect("a free permit");
        let context = RequestContext {
            connection: node_connection.clone(),
            transient_addr: None,
            genesis_hash: network.genesis_hash(),
            misbehavior: MisbehaviorScores::spawn(misbehavior_tx, 8),
            events,
            object_sizes: crate::peer::v2::connection::outbound::ObjectSizes::spawn(),
        };

        // The test runtime is single-threaded, so the task cannot run before it is aborted.
        let mut tasks = JoinSet::new();
        tasks.spawn(drive(request, ResponseGuard::new(tx), permit, context));
        tasks.abort_all();
        let joined = tasks.join_next().await.expect("one task was spawned");
        assert!(
            joined.as_ref().is_err_and(|error| error.is_cancelled()),
            "the task is cancelled without panicking: {joined:?}"
        );

        let error = rx
            .await
            .expect("the client is answered")
            .expect_err("an aborted request fails");
        assert!(
            error.inner_debug().contains("ConnectionDropped"),
            "got {}",
            error.inner_debug()
        );
        assert_eq!(
            permits.available_permits(),
            1,
            "the aborted task releases its permit"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A `get-object` stream from a peer is refused with `REFUSED` and the
/// connection stays open, and object requests are routed only to version 2
/// peers that advertise `NODE_SYNC_ARTIFACTS`.
#[tokio::test]
async fn get_object_is_refused_and_routed_by_service_bit() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        // A raw peer that advertises artifact serving, connected to a node serving the fixture.
        let (connection, inbound, _client_ep, _server_ep) = connected_pair(&network).await;
        let raw_init = InitRecord {
            version: crate::constants::CURRENT_NETWORK_PROTOCOL_VERSION,
            services: PeerServices::NODE_NETWORK | PeerServices::NODE_SYNC_ARTIFACTS,
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
            .write_all(&RecordCodec::encode_record(raw_init.encode()).expect("valid"))
            .await
            .expect("writes");
        let (b, nonces) = (identity("/b/"), NonceRegistry::spawn(8));
        let (completed, _hs_recv) = tokio::join!(
            run_handshake(&inbound, Role::Responder, &network, &b, &nonces),
            async {
                let mut framed = tokio_util::codec::FramedRead::new(hs_recv, RecordCodec);
                framed.next().await.expect("init").expect("valid");
                framed.into_inner()
            }
        );
        let node = spawn_node(
            inbound,
            completed.expect("the handshake completes"),
            Fixture::new(),
        );

        // The peer's `get-object` stream, with a full-length body: REFUSED, connection open.
        let (mut send, mut recv) = connection.open_bi().await.expect("opens");
        send.write_all(&[StreamType::GetObject.code()])
            .await
            .expect("writes");
        send.write_all(&[0; 44]).await.expect("writes");
        send.finish().expect("finishes");
        let error = recv.read_to_end(16).await.expect_err("refused");
        assert!(
            matches!(error, quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))
                if code.into_inner() == ErrorCode::Refused.code()),
            "got {error:?}"
        );
        assert!(
            connection.close_reason().is_none(),
            "the connection stays open"
        );

        let request = Request::Object {
            hash: zebra_chain::state_snapshot::ObjectHash([0xAB; 32]),
            offset: 0,
            length: 1,
        };

        // Routing: a version 2 peer with the bit supports object requests but not the mempool
        // snapshot; one without the bit supports neither.
        let tracked = crate::peer::LoadTrackedClient::from(node.client);
        assert!(tracked.supports(&request));
        assert!(!tracked.supports(&Request::MempoolTransactionIds));
        // Nor the legacy address request: its records go to the version 2 address segment.
        assert!(!tracked.supports(&Request::Peers));
        drop(hs_send);

        let without_bit = raw_peer_node(&network).await;
        let tracked = crate::peer::LoadTrackedClient::from(without_bit.node.client);
        assert!(!tracked.supports(&request));
        assert!(!tracked.supports(&Request::MempoolTransactionIds));
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A violation this node detects in a response, including the close that
/// bans the peer at the ban threshold, reaches the connection task through
/// the connection-closing queue however many best-effort events are waiting,
/// and handing it over never waits: a peer flooding address announcements
/// cannot hold back its own ban.
#[tokio::test]
async fn response_violations_are_never_crowded_out() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let fixture = Fixture::new();
        let genesis = fixture.blocks[0].hash();
        let block_1 = fixture.blocks[1].clone();
        let block_2 = fixture.blocks[2].clone();
        let mut non_contiguous = Vec::new();
        write_headers_response(
            &mut non_contiguous,
            &[block_2.header.clone(), block_1.header.clone()],
            false,
        )
        .expect("writes to a Vec");

        // The request, the raw peer's reply, and the code the violation closes with.
        for (request, reply, code) in [
            (
                Request::BlockDownload {
                    hash: block_1.hash(),
                    advertiser: None,
                },
                found(&block_2),
                ErrorCode::ProtocolError,
            ),
            (
                Request::FindHeaders {
                    known_blocks: vec![genesis],
                    stop: None,
                },
                non_contiguous,
                ErrorCode::Misbehavior,
            ),
        ] {
            let (node_connection, raw_connection, _e1, _e2) = connected_pair(&network).await;
            let raw_addr: PeerSocketAddr = node_connection.remote_address().into();
            let (misbehavior_tx, mut misbehavior_rx) = tokio::sync::mpsc::channel(8);
            let misbehavior = MisbehaviorScores::spawn(misbehavior_tx, 8);
            if code == ErrorCode::Misbehavior {
                // One misbehaving response short of the ban threshold.
                let penalties_to_ban = crate::constants::MAX_PEER_MISBEHAVIOR_SCORE
                    .div_ceil(crate::protocol::v2::constants::RESPONSE_MISBEHAVIOR_PENALTY);
                for _ in 1..penalties_to_ban {
                    let banned = misbehavior
                        .penalize(
                            raw_addr,
                            crate::protocol::v2::constants::RESPONSE_MISBEHAVIOR_PENALTY,
                            "earlier",
                        )
                        .await;
                    assert!(!banned, "the score stays below the threshold");
                }
            }

            // The best-effort queue is full of the peer's addresses, and nothing reads it.
            let (events, mut fatal_rx, mut events_rx) = Events::channel();
            for _ in 0..EVENT_QUEUE {
                events.addresses(fixture.addrs.clone());
            }

            let (tx, rx) = futures::channel::oneshot::channel();
            let tx = MustUseClientResponseSender::new(tx, &request, None, None);
            let permits = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = permits.clone().try_acquire_owned().expect("a free permit");
            let context = RequestContext {
                connection: node_connection.clone(),
                transient_addr: Some(raw_addr),
                genesis_hash: network.genesis_hash(),
                misbehavior,
                events,
                object_sizes: ObjectSizes::spawn(),
            };
            let (driven, ()) = tokio::join!(
                tokio::time::timeout(
                    SHORT_WAIT * 4,
                    drive(request, ResponseGuard::new(tx), permit, context)
                ),
                async {
                    let (_, mut send, _recv) = accept_request(&raw_connection).await;
                    let _ = send.write_all(&reply).await;
                    let _ = send.finish();
                }
            );
            driven.unwrap_or_else(|_elapsed| {
                panic!("{code:?}: handing over the violation never waits")
            });

            let error = rx
                .await
                .expect("the client is answered")
                .expect_err("a violation fails the request");
            assert!(
                error.inner_debug().contains("ProtocolViolation"),
                "{code:?}: got {}",
                error.inner_debug()
            );
            assert!(
                matches!(
                    fatal_rx.try_recv(),
                    Ok(Event::Violation { code: queued, .. }) if queued == code
                ),
                "{code:?}: the violation is queued for the connection task"
            );
            assert_eq!(
                misbehavior_rx.try_recv().is_ok(),
                code == ErrorCode::Misbehavior,
                "{code:?}: the peer is reported for banning at the threshold, and only then"
            );
            let mut queued = 0;
            while let Ok(event) = events_rx.try_recv() {
                assert!(matches!(event, Event::Addresses(_)));
                queued += 1;
            }
            assert_eq!(
                queued, EVENT_QUEUE,
                "{code:?}: the address events are untouched"
            );
        }
    })
    .await
    .expect("the test completes well within the timeout");
}

/// One address announcement record for `addr`.
fn addr_record(addr: &MetaAddr) -> Bytes {
    RecordCodec::encode_record(encode_addr_record(addr).expect("encodes")).expect("valid")
}

/// A gossiped address with a fresh time.
fn fresh_addr(addr: &str) -> MetaAddr {
    MetaAddr::new_gossiped_meta_addr(
        addr.parse::<SocketAddr>().unwrap().into(),
        PeerServices::NODE_NETWORK,
        DateTime32::now(),
    )
}

/// Opens an address announcement stream from `connection` and writes
/// `addrs` on it, one record each, returning the stream to keep it open.
async fn announce_addrs(connection: &Connection, addrs: &[MetaAddr]) -> SendStream {
    let mut send = connection.open_uni().await.expect("opens");
    send.write_all(&[StreamType::AddressAnnouncements.code()])
        .await
        .expect("writes");
    for addr in addrs {
        send.write_all(&addr_record(addr)).await.expect("writes");
    }
    send
}

/// Accepts the address announcement stream the node opens to `connection`,
/// and returns its records as they arrive.
async fn accept_addr_stream(
    connection: &Connection,
) -> tokio_util::codec::FramedRead<RecvStream, RecordCodec> {
    let mut recv = connection
        .accept_uni()
        .await
        .expect("the node opens a stream");
    let mut type_byte = [0u8; 1];
    recv.read_exact(&mut type_byte).await.expect("reads");
    assert_eq!(type_byte[0], StreamType::AddressAnnouncements.code());
    tokio_util::codec::FramedRead::new(recv, RecordCodec)
}

/// A hub that trickles relayed addresses out at once, and advertises
/// nothing.
fn fast_hub() -> AddrRelayHandle {
    AddrRelay::spawn_with(
        None,
        Network::Mainnet,
        [3; 32],
        Timings {
            trickle_mean: Duration::from_millis(10),
            ..Timings::default()
        },
    )
}

/// Announced addresses beyond the peer's budget are dropped silently: the
/// connection stays open, and only the first record is relayed.
#[tokio::test]
async fn announced_addresses_over_budget_are_dropped_without_an_error() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let hub = fast_hub();
        let relaying = || Relaying {
            addr_relay: Some(hub.clone()),
            ..Relaying::default()
        };
        let mut raw = raw_peer_node_with_hub(&network, relaying()).await;
        let destination = raw_peer_node_with_hub(&network, relaying()).await;

        let addrs: Vec<MetaAddr> = (1..=5)
            .map(|index| fresh_addr(&format!("203.0.113.{index}:8233")))
            .collect();
        let _send = announce_addrs(&raw.connection, &addrs).await;

        let mut records = accept_addr_stream(&destination.connection).await;
        let mut received = Vec::new();
        while let Ok(Some(payload)) = tokio::time::timeout(SHORT_WAIT, records.next()).await {
            let payload = payload.expect("a valid record");
            received.extend(decode_addr_record(&payload).expect("decodes"));
        }
        assert_eq!(
            received.iter().map(MetaAddr::addr).collect::<Vec<_>>(),
            vec![addrs[0].addr()],
            "only the first record is within the budget"
        );

        assert!(
            call(&mut raw.node.client, Request::Ping(Default::default()))
                .await
                .is_ok(),
            "the connection is still open"
        );
        assert!(raw.connection.close_reason().is_none());
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An address one peer announces is relayed to another peer on the node's
/// address announcement stream, in a record `decode_addr_record` reads.
#[tokio::test]
async fn announced_addresses_are_relayed_to_other_peers() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let hub = fast_hub();
        let relaying = || Relaying {
            addr_relay: Some(hub.clone()),
            ..Relaying::default()
        };
        // Both connections register before anything is offered.
        let source = raw_peer_node_with_hub(&network, relaying()).await;
        let destination = raw_peer_node_with_hub(&network, relaying()).await;

        let addr = fresh_addr("203.0.113.9:8233");
        let _send = announce_addrs(&source.connection, std::slice::from_ref(&addr)).await;

        let mut records = accept_addr_stream(&destination.connection).await;
        let payload = records
            .next()
            .await
            .expect("a record")
            .expect("a valid record");
        assert_eq!(decode_addr_record(&payload).expect("decodes"), Some(addr));

        // Nothing comes back to the source.
        assert!(
            tokio::time::timeout(SHORT_WAIT, source.connection.accept_uni())
                .await
                .is_err(),
            "the source is not sent its own address"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A node relays what it accepts from a burst of more than ten address
/// records written back to back, as another node's hub flush writes them:
/// how many arrive together is network timing, not news, so it doesn't stop
/// the next hop. A new connection's budget keeps only the first record.
#[tokio::test]
async fn back_to_back_address_records_are_still_relayed() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let hub = fast_hub();
        let relaying = || Relaying {
            addr_relay: Some(hub.clone()),
            ..Relaying::default()
        };
        let source = raw_peer_node_with_hub(&network, relaying()).await;
        let destination = raw_peer_node_with_hub(&network, relaying()).await;

        let addrs: Vec<MetaAddr> = (1..=11)
            .map(|index| fresh_addr(&format!("203.0.113.{index}:8233")))
            .collect();
        let mut send = source.connection.open_uni().await.expect("opens");
        let mut burst = vec![StreamType::AddressAnnouncements.code()];
        for addr in &addrs {
            burst.extend_from_slice(&addr_record(addr));
        }
        send.write_all(&burst).await.expect("writes");

        let mut records = accept_addr_stream(&destination.connection).await;
        let payload = records
            .next()
            .await
            .expect("a record")
            .expect("a valid record");
        assert_eq!(
            decode_addr_record(&payload).expect("decodes"),
            Some(addrs[0].clone()),
            "the record within the budget is relayed"
        );
        assert!(
            tokio::time::timeout(SHORT_WAIT, records.next())
                .await
                .is_err(),
            "the records over the budget are not"
        );
        drop(send);
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Banned addresses a peer announces are not relayed.
#[tokio::test]
async fn banned_addresses_are_never_relayed() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let banned = fresh_addr("203.0.113.66:8233");
        let mut ban_list = BanList::default();
        ban_list.ban(banned.addr().ip());
        let (_bans_tx, bans) = tokio::sync::watch::channel(ban_list);

        let hub = fast_hub();
        let relaying = || Relaying {
            addr_relay: Some(hub.clone()),
            bans: bans.clone(),
            address_book: None,
        };
        let banned_source = raw_peer_node_with_hub(&network, relaying()).await;
        let destination = raw_peer_node_with_hub(&network, relaying()).await;
        // Each connection has its own budget, so the allowed address comes from another peer.
        let source = raw_peer_node_with_hub(&network, relaying()).await;

        let _banned_send =
            announce_addrs(&banned_source.connection, std::slice::from_ref(&banned)).await;
        tokio::time::sleep(SHORT_WAIT).await;
        let allowed = fresh_addr("203.0.113.67:8233");
        let _send = announce_addrs(&source.connection, std::slice::from_ref(&allowed)).await;

        let mut records = accept_addr_stream(&destination.connection).await;
        let payload = records
            .next()
            .await
            .expect("a record")
            .expect("a valid record");
        assert_eq!(
            decode_addr_record(&payload).expect("decodes"),
            Some(allowed),
            "the first relayed record is the allowed address, not the banned one"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// Opens a `get-hashes` request stream for `count` heights from `start`.
///
/// The body is written by hand: `start_height u32 LE ‖ count u32 LE`.
async fn raw_get_hashes(connection: &Connection, start: u32, count: u32) -> RecvStream {
    let (mut send, recv) = connection.open_bi().await.expect("opens");
    let mut request = vec![StreamType::GetHashes.code()];
    request.extend_from_slice(&start.to_le_bytes());
    request.extend_from_slice(&count.to_le_bytes());
    send.write_all(&request).await.expect("writes");
    send.finish().expect("finishes");
    recv
}

/// `n` distinct `get-hashes` entries.
fn sync_hash_entries(n: u32) -> Vec<block::SyncHashEntry> {
    (0..n)
        .map(|k| block::SyncHashEntry {
            hash: block::Hash([u8::try_from(k % 256).expect("in range"); 32]),
            size: 1_000 + u64::from(k),
            txs: 1,
            // Above one byte, so the CompactSize encoding is exercised.
            txouts: 300 + u64::from(k),
            notes: 0,
        })
        .collect()
}

/// A `get-hashes` request reaches the inbound service as
/// `Request::SyncHashes`, and its answer is sent as one response body; an
/// empty answer is a response with no entries, and an answer longer than the
/// request is cut to the requested prefix.
#[tokio::test]
async fn get_hashes_is_served_from_the_inbound_service() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requests_tx, mut requests_rx) = tokio::sync::mpsc::unbounded_channel();
        let service = service_fn(move |request: Request| {
            let _ = requests_tx.send(request.clone());
            async move {
                match request {
                    // Answers with 7 entries whatever the count, to check the cut.
                    Request::SyncHashes {
                        start: block::Height(10),
                        ..
                    } => Ok(Response::SyncHashes(sync_hash_entries(7))),
                    Request::SyncHashes { .. } => Ok(Response::SyncHashes(Vec::new())),
                    _ => Err::<Response, BoxError>("unexpected request".into()),
                }
            }
        });
        let raw = raw_peer_node_with(&network, Some(BoxCloneService::new(service))).await;

        for (count, expected) in [(7, 7), (100, 7), (3, 3)] {
            let mut recv = raw_get_hashes(&raw.connection, 10, count).await;
            let body = recv
                .read_to_end(MAX_HASHES_RESPONSE_LEN)
                .await
                .expect("a complete response");
            let request = GetHashes {
                start_height: block::Height(10),
                count,
            };
            let entries = decode_hashes_response(&body, &request).expect("a conforming response");
            assert_eq!(entries, sync_hash_entries(expected), "count {count}");
            assert_eq!(
                requests_rx.recv().await,
                Some(Request::SyncHashes {
                    start: block::Height(10),
                    count,
                })
            );
        }

        // Nothing to serve: a response with no entries, not a refusal.
        let mut recv = raw_get_hashes(&raw.connection, 1_000_000, 50).await;
        let body = recv.read_to_end(16).await.expect("a complete response");
        assert_eq!(body, [0], "a zero count and no entries");

        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// One peer's `get-hashes` requests beyond `HASHES_STREAMS_PER_PEER` are
/// refused at once, while the peer's other requests are still served from
/// their own permits.
#[tokio::test]
async fn excess_get_hashes_streams_are_refused() {
    use super::super::inbound::hashes::HASHES_STREAMS_PER_PEER;

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        // `get-hashes` lookups wait until released; everything else is answered from the fixture.
        let (release_tx, release_rx) = tokio::sync::watch::channel(false);
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let fixture = Fixture::new();
        let service = service_fn(move |request: Request| {
            let fixture = fixture.clone();
            let mut release_rx = release_rx.clone();
            let started_tx = started_tx.clone();
            async move {
                if let Request::SyncHashes { .. } = request {
                    let _ = started_tx.send(());
                    release_rx
                        .wait_for(|released| *released)
                        .await
                        .expect("the test keeps the sender");
                    return Ok(Response::SyncHashes(sync_hash_entries(1)));
                }
                Ok::<Response, BoxError>(fixture.answer(request))
            }
        });
        let raw = raw_peer_node_with(&network, Some(BoxCloneService::new(service))).await;

        // The peer's share of `get-hashes` permits, all waiting on the inbound service.
        let mut held = Vec::new();
        for _ in 0..HASHES_STREAMS_PER_PEER {
            held.push(raw_get_hashes(&raw.connection, 10, 1).await);
            started_rx.recv().await.expect("the lookup starts");
        }

        // One more is refused at once, and never reaches the inbound service.
        let mut excess = raw_get_hashes(&raw.connection, 10, 1).await;
        assert_reset(excess.read_to_end(1024).await, ErrorCode::Refused);
        assert!(
            started_rx.try_recv().is_err(),
            "the refused lookup never starts"
        );

        // The peer's other requests are not held up behind its `get-hashes` requests.
        let mut recv = raw_request(&raw.connection, &WireRequest::Mempool).await;
        recv.read_to_end(1 << 20)
            .await
            .expect("get-mempool is served while get-hashes permits are taken");

        // Released, the waiting requests complete, and their permits are free again.
        release_tx.send(true).expect("the service keeps a receiver");
        for mut recv in held {
            recv.read_to_end(MAX_HASHES_RESPONSE_LEN)
                .await
                .expect("a complete response");
        }
        let mut recv = raw_get_hashes(&raw.connection, 10, 1).await;
        recv.read_to_end(MAX_HASHES_RESPONSE_LEN)
            .await
            .expect("a complete response");

        assert!(
            raw.connection.close_reason().is_none(),
            "the connection stays open"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}
