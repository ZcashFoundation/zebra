//! Tests for serving `get-block-range` requests: a raw requester on one end of
//! a QUIC connection, and the request stream server on the other, driven
//! directly with a test inbound service that answers block range reads from a
//! fixed chain.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use quinn::{Connection, RecvStream};
use tokio::{sync::Semaphore, task::JoinHandle, time::Instant};
use tower::{service_fn, util::BoxCloneService};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashSerialize},
};

use crate::{
    constants::REQUEST_TIMEOUT,
    peer::v2::{
        connection::{
            inbound::{
                block_range::{
                    RANGE_READ_CHUNK, RANGE_READ_CHUNK_BLOCKS, RANGE_STREAMS_GLOBAL,
                    RANGE_STREAMS_PER_PEER,
                },
                global_block_range_permits, serve as serve_request_stream, BlockRangeServing,
                INBOUND_REQUEST_CONCURRENCY,
            },
            Events, Overloads,
        },
        tests::connected_pair,
    },
    protocol::{
        internal::{Request, Response},
        v2::{
            request::{GetBlockRange, Request as WireRequest},
            types::ErrorCode,
        },
    },
    BoxError,
};

/// Every exchange here finishes in well under this, except the deadline tests,
/// which add their own waits.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a test waits to see that something does not happen.
const SHORT_WAIT: Duration = Duration::from_millis(500);

/// The largest response a test reads.
const MAX_TEST_RESPONSE: usize = 64 * 1024 * 1024;

/// The continuous mainnet test vector chain, genesis first.
fn chain() -> Vec<Arc<Block>> {
    zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| Arc::new(Block::zcash_deserialize(*bytes).expect("valid vector")))
        .collect()
}

/// The serialized length of `block`.
fn len(block: &Block) -> u32 {
    block
        .zcash_serialize_to_vec()
        .expect("serializes")
        .len()
        .try_into()
        .expect("test blocks are small")
}

/// How the test inbound service answers block range reads.
#[derive(Clone, Default)]
struct ChainService {
    /// The blocks it knows, by hash.
    blocks: HashMap<block::Hash, Arc<Block>>,
    /// The most blocks one read returns, if fewer than the request's `count`.
    per_call: Option<usize>,
    /// Blocks it does not have, as if pruned.
    missing: HashSet<block::Hash>,
    /// How long each read waits before answering.
    delay: Duration,
    /// Reads after this many never answer.
    answered_calls: Option<usize>,
    /// Reads, by their number from zero, that answer wrongly.
    faults: HashMap<usize, Fault>,
}

/// A wrong answer to a block range read.
#[derive(Clone, Copy, Debug)]
enum Fault {
    /// `Response::Nil`, as an inbound service that is still starting answers.
    Nil,
    /// An error.
    Error,
    /// A block that is not the requested one.
    WrongBlock,
}

/// A test inbound service.
type TestService = BoxCloneService<Request, Response, BoxError>;

impl ChainService {
    fn new(chain: &[Arc<Block>]) -> Self {
        Self {
            blocks: chain.iter().map(|b| (b.hash(), b.clone())).collect(),
            ..Self::default()
        }
    }

    /// Answers like `ReadRequest::AncestorBlocks`: the ancestor chain of
    /// `from_hash`, at most `count` blocks, and at most `max_bytes` bytes
    /// after the first block.
    fn read(&self, from_hash: block::Hash, count: u32, max_bytes: u32) -> Vec<Arc<Block>> {
        let mut blocks = Vec::new();
        let mut bytes: u64 = 0;
        let mut next = from_hash;
        let limit = self.per_call.unwrap_or(usize::MAX).min(count as usize);
        while blocks.len() < limit {
            if self.missing.contains(&next) {
                break;
            }
            let Some(block) = self.blocks.get(&next) else {
                break;
            };
            let size = u64::from(len(block));
            if !blocks.is_empty() && bytes + size > u64::from(max_bytes) {
                break;
            }
            bytes += size;
            next = block.header.previous_block_hash;
            blocks.push(block.clone());
        }
        blocks
    }

    /// The service, and the receiver its requests are copied to.
    fn service(self) -> (TestService, tokio::sync::mpsc::UnboundedReceiver<Request>) {
        let (requests_tx, requests_rx) = tokio::sync::mpsc::unbounded_channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let service = service_fn(move |request: Request| {
            let this = self.clone();
            let requests_tx = requests_tx.clone();
            let calls = calls.clone();
            async move {
                // Other requests are answered at once, and not recorded.
                let Request::BlockRange {
                    from_hash,
                    count,
                    max_bytes,
                } = request
                else {
                    return Ok::<_, BoxError>(Response::Nil);
                };
                let _ = requests_tx.send(request);
                let call = calls.fetch_add(1, Ordering::SeqCst);
                if this.answered_calls.is_some_and(|answered| call >= answered) {
                    std::future::pending::<()>().await;
                }
                tokio::time::sleep(this.delay).await;
                match this.faults.get(&call) {
                    None => Ok(Response::BlockRange(this.read(from_hash, count, max_bytes))),
                    Some(Fault::Nil) => Ok(Response::Nil),
                    Some(Fault::Error) => Err("test read failure".into()),
                    Some(Fault::WrongBlock) => {
                        let wrong = this
                            .blocks
                            .values()
                            .find(|block| block.hash() != from_hash)
                            .expect("the test chain has more than one block")
                            .clone();
                        Ok(Response::BlockRange(vec![wrong]))
                    }
                }
            }
        });
        (BoxCloneService::new(service), requests_rx)
    }
}

/// Serves every stream `responder` accepts with `service` and `serving`, as
/// one connection's stream tasks do, until the connection closes.
fn spawn_server(
    responder: Connection,
    service: TestService,
    serving: BlockRangeServing,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(INBOUND_REQUEST_CONCURRENCY));
        let overloads = Arc::new(Overloads::new());
        let get_addr_allowed = Arc::new(AtomicBool::new(false));
        let (events, _fatal_rx, _events_rx) = Events::channel();
        let mut tasks = tokio::task::JoinSet::new();
        while let Ok((send, recv)) = responder.accept_bi().await {
            tasks.spawn(serve_request_stream(
                send,
                recv,
                service.clone(),
                permits.clone(),
                overloads.clone(),
                get_addr_allowed.clone(),
                None,
                events.clone(),
                serving.clone(),
            ));
        }
    })
}

/// Sends a `get-block-range` request on a new stream of `requester`.
async fn request_range(requester: &Connection, request: GetBlockRange) -> RecvStream {
    let request = WireRequest::BlockRange(request);
    let (mut send, recv) = requester.open_bi().await.expect("opens");
    send.write_all(&[request.stream_type().code()])
        .await
        .expect("writes");
    send.write_all(&request.encode().expect("encodes"))
        .await
        .expect("writes");
    send.finish().expect("finishes");
    recv
}

/// A complete `get-block-range` response, parsed from its bytes.
#[derive(Debug, Eq, PartialEq)]
enum Parsed {
    Found(Vec<block::Hash>),
    NotFound,
}

/// Parses a complete response: a `result` byte, then, if found,
/// `CompactSize len ‖ block` elements up to the end.
fn parse(bytes: &[u8]) -> Parsed {
    let (&result, mut rest) = bytes.split_first().expect("a result byte");
    match result {
        0x02 => {
            assert!(rest.is_empty(), "nothing follows a not-found result");
            Parsed::NotFound
        }
        0x00 => {
            let mut hashes = Vec::new();
            while !rest.is_empty() {
                let (len, prefix) = match rest[0] {
                    0xFD => (usize::from(u16::from_le_bytes([rest[1], rest[2]])), 3),
                    0xFE => (
                        u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize,
                        5,
                    ),
                    0xFF => panic!("no test block needs an 8-byte length"),
                    small => (usize::from(small), 1),
                };
                let (element, tail) = rest[prefix..].split_at(len);
                let block = Block::zcash_deserialize(element).expect("an element is one block");
                hashes.push(block.hash());
                rest = tail;
            }
            Parsed::Found(hashes)
        }
        other => panic!("unexpected result byte {other:#04x}"),
    }
}

/// Reads a whole response from `recv` and parses it.
async fn read_response(mut recv: RecvStream) -> Parsed {
    let bytes = recv
        .read_to_end(MAX_TEST_RESPONSE)
        .await
        .expect("the response is finished");
    parse(&bytes)
}

/// The hashes of `chain[from]`, `chain[from - 1]`, … `chain[to]`.
fn descending(chain: &[Arc<Block>], from: usize, to: usize) -> Vec<block::Hash> {
    (to..=from)
        .rev()
        .map(|height| chain[height].hash())
        .collect()
}

/// Asserts that a read of `recv` ends in a reset with `code`.
async fn assert_reset(mut recv: RecvStream, code: ErrorCode) {
    let error = recv
        .read_to_end(MAX_TEST_RESPONSE)
        .await
        .expect_err("the stream is reset");
    assert!(
        matches!(&error, quinn::ReadToEndError::Read(quinn::ReadError::Reset(reset))
            if reset.into_inner() == code.code()),
        "expected a {code:?} reset, got {error:?}"
    );
}

/// The block range reads the service received, without waiting.
fn reads(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<Request>,
) -> Vec<(block::Hash, u32, u32)> {
    std::iter::from_fn(|| requests.try_recv().ok())
        .map(|request| match request {
            Request::BlockRange {
                from_hash,
                count,
                max_bytes,
            } => (from_hash, count, max_bytes),
            other => panic!("unexpected request {other:?}"),
        })
        .collect()
}

/// A range is served highest block first, down to genesis, where it ends
/// early; the first read asks for one chunk.
#[tokio::test]
async fn block_ranges_are_served_down_to_genesis() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let tip = chain.len() - 1;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let (service, mut requests) = ChainService::new(&chain).service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits()),
        );

        let recv = request_range(
            &requester,
            GetBlockRange {
                final_hash: chain[tip].hash(),
                count: 65_536,
                max_bytes: 64 * 1024 * 1024,
            },
        )
        .await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, 0))
        );
        assert_eq!(
            reads(&mut requests),
            vec![(chain[tip].hash(), RANGE_READ_CHUNK_BLOCKS, RANGE_READ_CHUNK)],
            "the whole chain fits in one chunk"
        );
    })
    .await
    .expect("the test completes");
}

/// A range stops at the request's `count`, and at its `max_bytes`, which the
/// first block is exempt from; a `count` of zero is found with no blocks.
#[tokio::test]
async fn block_ranges_stop_at_the_request_bounds() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let tip = chain.len() - 1;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let (service, mut requests) = ChainService::new(&chain).service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits()),
        );
        let range = |count, max_bytes| GetBlockRange {
            final_hash: chain[tip].hash(),
            count,
            max_bytes,
        };

        // `count` is exact.
        let recv = request_range(&requester, range(3, 64 * 1024 * 1024)).await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip - 2))
        );

        // The first block is sent whatever `max_bytes` is, and the read asks for no more than
        // `max_bytes`, rather than a whole chunk.
        let _ = reads(&mut requests);
        let recv = request_range(&requester, range(65_536, 1)).await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip))
        );
        assert_eq!(
            reads(&mut requests),
            vec![(chain[tip].hash(), RANGE_READ_CHUNK_BLOCKS, 1)],
            "one read, bounded by max_bytes"
        );

        // `max_bytes` counts the first block, and is exact.
        let two = len(&chain[tip]) + len(&chain[tip - 1]);
        let recv = request_range(&requester, range(100, two)).await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip - 1))
        );
        let recv = request_range(&requester, range(100, two - 1)).await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip))
        );

        // No blocks wanted: found, and nothing is read.
        let _ = reads(&mut requests);
        let recv = request_range(&requester, range(0, 64 * 1024 * 1024)).await;
        assert_eq!(read_response(recv).await, Parsed::Found(Vec::new()));
        assert!(reads(&mut requests).is_empty(), "nothing is read");
    })
    .await
    .expect("the test completes");
}

/// A range is read in chunks, each continuing from the last block's parent,
/// with the count left and the bytes left of the request, the first read
/// included.
#[tokio::test]
async fn block_ranges_are_read_in_chunks() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let tip = chain.len() - 1;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.per_call = Some(2);
        let (service, mut requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits()),
        );

        let max_bytes = 1_000_000;
        let recv = request_range(
            &requester,
            GetBlockRange {
                final_hash: chain[tip].hash(),
                count: 5,
                max_bytes,
            },
        )
        .await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip - 4))
        );

        let sent = |height: usize| -> u32 { ((height + 1)..=tip).map(|h| len(&chain[h])).sum() };
        assert_eq!(
            reads(&mut requests),
            vec![
                (chain[tip].hash(), 5, max_bytes),
                (chain[tip - 2].hash(), 3, max_bytes - sent(tip - 2)),
                (chain[tip - 4].hash(), 1, max_bytes - sent(tip - 4)),
            ]
        );
    })
    .await
    .expect("the test completes");
}

/// An unknown final hash is not found; a missing ancestor finishes the
/// response early with the blocks above it.
#[tokio::test]
async fn block_ranges_end_where_the_blocks_end() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let tip = chain.len() - 1;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.per_call = Some(1);
        chain_service.missing.insert(chain[tip - 3].hash());
        let (service, _requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits()),
        );

        let recv = request_range(
            &requester,
            GetBlockRange {
                final_hash: block::Hash([0xAB; 32]),
                count: 10,
                max_bytes: 1_000_000,
            },
        )
        .await;
        assert_eq!(read_response(recv).await, Parsed::NotFound);

        let recv = request_range(
            &requester,
            GetBlockRange {
                final_hash: chain[tip].hash(),
                count: 10,
                max_bytes: 1_000_000,
            },
        )
        .await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip - 2))
        );
    })
    .await
    .expect("the test completes");
}

/// With serving disabled, every range stream is refused, before anything is
/// read.
#[tokio::test]
async fn disabled_block_range_serving_refuses_streams() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let (service, mut requests) = ChainService::new(&chain).service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(false, global_block_range_permits()),
        );

        let recv = request_range(
            &requester,
            GetBlockRange {
                final_hash: chain[1].hash(),
                count: 1,
                max_bytes: 0,
            },
        )
        .await;
        assert_reset(recv, ErrorCode::Refused).await;
        assert!(reads(&mut requests).is_empty(), "nothing is read");
    })
    .await
    .expect("the test completes");
}

/// A request for the tip of `chain`.
fn tip_range(chain: &[Arc<Block>]) -> GetBlockRange {
    GetBlockRange {
        final_hash: chain[chain.len() - 1].hash(),
        count: 10,
        max_bytes: 1_000_000,
    }
}

/// Waits until the service has received `count` requests.
async fn wait_for_reads(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<Request>,
    count: usize,
) {
    for _ in 0..count {
        requests.recv().await.expect("the service is running");
    }
}

/// A peer's range streams beyond [`RANGE_STREAMS_PER_PEER`] are refused at
/// once, not queued, and its other requests are still served.
#[tokio::test]
async fn excess_block_range_streams_from_one_peer_are_refused() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.answered_calls = Some(0);
        let (service, mut requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits()),
        );

        let mut held = Vec::new();
        for _ in 0..RANGE_STREAMS_PER_PEER {
            held.push(request_range(&requester, tip_range(&chain)).await);
        }
        wait_for_reads(&mut requests, RANGE_STREAMS_PER_PEER).await;

        let started = Instant::now();
        let refused = request_range(&requester, tip_range(&chain)).await;
        assert_reset(refused, ErrorCode::Refused).await;
        assert!(started.elapsed() < REQUEST_TIMEOUT / 2, "refused at once");

        // The range streams hold none of the permits of the other request types, which are
        // still served.
        let (mut send, mut recv) = requester.open_bi().await.expect("opens");
        send.write_all(&[WireRequest::Mempool.stream_type().code()])
            .await
            .expect("writes");
        send.finish().expect("finishes");
        recv.read_to_end(1024)
            .await
            .expect("a get-mempool request is still served");

        tokio::time::sleep(SHORT_WAIT).await;
        assert!(
            reads(&mut requests).is_empty(),
            "the refused stream made no read"
        );
        drop(held);
    })
    .await
    .expect("the test completes");
}

/// Over all peers, range streams beyond [`RANGE_STREAMS_GLOBAL`] are refused.
#[tokio::test]
async fn excess_block_range_streams_over_all_peers_are_refused() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let global = global_block_range_permits();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let mut chain_service = ChainService::new(&chain);
        chain_service.answered_calls = Some(0);
        let (service, mut requests) = chain_service.service();

        // Enough peers to use every node permit, each with all its own permits in use.
        let peers = RANGE_STREAMS_GLOBAL / RANGE_STREAMS_PER_PEER;
        let mut held = Vec::new();
        for _ in 0..peers {
            let (requester, responder, client_ep, server_ep) = connected_pair(&network).await;
            let server = spawn_server(
                responder,
                service.clone(),
                BlockRangeServing::new(true, global.clone()),
            );
            let mut streams = Vec::new();
            for _ in 0..RANGE_STREAMS_PER_PEER {
                streams.push(request_range(&requester, tip_range(&chain)).await);
            }
            held.push((requester, streams, server, client_ep, server_ep));
        }
        wait_for_reads(&mut requests, RANGE_STREAMS_GLOBAL).await;

        // A new peer, with all of its own permits free, is refused.
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let _server = spawn_server(
            responder,
            service.clone(),
            BlockRangeServing::new(true, global.clone()),
        );
        let refused = request_range(&requester, tip_range(&chain)).await;
        assert_reset(refused, ErrorCode::Refused).await;
        tokio::time::sleep(SHORT_WAIT).await;
        assert!(
            reads(&mut requests).is_empty(),
            "the refused stream made no read"
        );
        drop(held);
    })
    .await
    .expect("the test completes");
}

/// A range response whose next chunk is not written within
/// [`REQUEST_TIMEOUT`] is abandoned: reset with `CANCELLED`, since part of it
/// was written.
///
/// This takes `REQUEST_TIMEOUT` of real time: pausing tokio's clock is
/// unreliable with real sockets.
#[tokio::test]
async fn stalled_block_range_responses_are_cancelled() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let tip = chain.len() - 1;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.per_call = Some(1);
        chain_service.answered_calls = Some(1);
        let (service, _requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits()),
        );

        let started = Instant::now();
        let mut recv = request_range(&requester, tip_range(&chain)).await;

        // The first chunk is written: the result byte and the tip block.
        let tip_bytes = chain[tip].zcash_serialize_to_vec().expect("serializes");
        let mut first = vec![0; 1 + 3 + tip_bytes.len()];
        recv.read_exact(&mut first)
            .await
            .expect("the first chunk is delivered");
        assert_eq!(first[0], 0x00, "found");
        assert_eq!(&first[4..], &tip_bytes[..]);

        // The second never comes.
        assert_reset(recv, ErrorCode::Cancelled).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= REQUEST_TIMEOUT - Duration::from_secs(1),
            "the chunk had its whole deadline, but the stream was reset after {elapsed:?}"
        );
    })
    .await
    .expect("the test completes");
}

/// A range response that keeps making progress is not bounded by the
/// request deadline of the other request types.
///
/// This takes more than `REQUEST_TIMEOUT` of real time.
#[tokio::test]
async fn block_range_responses_outlive_the_request_deadline() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let tip = chain.len() - 1;
    let chunks = 3;
    let delay = REQUEST_TIMEOUT / 2;

    tokio::time::timeout(TEST_TIMEOUT + REQUEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.per_call = Some(1);
        chain_service.delay = delay;
        let (service, _requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            // A few small blocks every 10 s are below the minimum rate, which is not what this
            // test is about.
            BlockRangeServing::new(true, global_block_range_permits())
                .with_min_progress(0, REQUEST_TIMEOUT),
        );

        let started = Instant::now();
        let recv = request_range(
            &requester,
            GetBlockRange {
                final_hash: chain[tip].hash(),
                count: chunks,
                max_bytes: 1_000_000,
            },
        )
        .await;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip + 1 - chunks as usize))
        );
        assert!(
            started.elapsed() > REQUEST_TIMEOUT,
            "the response outlived the deadline"
        );
    })
    .await
    .expect("the test completes");
}

/// A first read that the inbound service does not answer with a block range,
/// as one that is still starting answers `Nil`, refuses the stream rather
/// than answer that this node does not have the block.
#[tokio::test]
async fn unanswered_first_block_range_reads_refuse_the_stream() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.faults.insert(0, Fault::Nil);
        let (service, _requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits()),
        );

        let recv = request_range(&requester, tip_range(&chain)).await;
        assert_reset(recv, ErrorCode::Refused).await;
    })
    .await
    .expect("the test completes");
}

/// Range streams that never send their request hold only their own peer's
/// permits: a peer that opens its share of streams and sends only their type
/// bytes cannot use up the node's permits, so other peers are still served.
#[tokio::test]
async fn block_range_streams_without_a_request_hold_no_node_permits() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let global = global_block_range_permits();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (service, _requests) = ChainService::new(&chain).service();

        // Enough peers to use every node permit, each with all its streams waiting for their
        // requests.
        let peers = RANGE_STREAMS_GLOBAL / RANGE_STREAMS_PER_PEER;
        let mut held = Vec::new();
        for _ in 0..peers {
            let (requester, responder, client_ep, server_ep) = connected_pair(&network).await;
            let server = spawn_server(
                responder,
                service.clone(),
                BlockRangeServing::new(true, global.clone()),
            );
            let mut streams = Vec::new();
            for _ in 0..RANGE_STREAMS_PER_PEER {
                let (mut send, recv) = requester.open_bi().await.expect("opens");
                send.write_all(&[WireRequest::BlockRange(tip_range(&chain))
                    .stream_type()
                    .code()])
                    .await
                    .expect("writes");
                streams.push((send, recv));
            }
            held.push((requester, streams, server, client_ep, server_ep));
        }
        // Let the server read every type byte.
        tokio::time::sleep(SHORT_WAIT).await;
        assert_eq!(
            global.available_permits(),
            RANGE_STREAMS_GLOBAL,
            "no node permit is held for a stream without a request"
        );

        // Each of those peers has used its own permits.
        let (requester, ..) = &held[0];
        let refused = request_range(requester, tip_range(&chain)).await;
        assert_reset(refused, ErrorCode::Refused).await;

        // A new peer is served.
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let _server = spawn_server(
            responder,
            service.clone(),
            BlockRangeServing::new(true, global.clone()),
        );
        let recv = request_range(&requester, tip_range(&chain)).await;
        let tip = chain.len() - 1;
        assert_eq!(
            read_response(recv).await,
            Parsed::Found(descending(&chain, tip, tip - 9))
        );
        drop(held);
    })
    .await
    .expect("the test completes");
}

/// A range response that is not complete within its whole-stream deadline is
/// abandoned, even if every chunk is in time: reset with `CANCELLED`, since
/// part of it was written.
///
/// The deadline is shortened from [`RANGE_STREAM_TIMEOUT`] for the test:
/// pausing tokio's clock is unreliable with real sockets.
///
/// [`RANGE_STREAM_TIMEOUT`]: crate::peer::v2::connection::inbound::block_range::RANGE_STREAM_TIMEOUT
#[tokio::test]
async fn slow_block_range_responses_are_cancelled_at_the_stream_deadline() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let stream_timeout = Duration::from_secs(2);

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.per_call = Some(1);
        chain_service.delay = Duration::from_millis(500);
        let (service, _requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits())
                .with_stream_timeout(stream_timeout),
        );

        // Ten chunks of half a second each: every chunk is well within its deadline, but the
        // whole response is not.
        let started = Instant::now();
        let recv = request_range(&requester, tip_range(&chain)).await;
        assert_reset(recv, ErrorCode::Cancelled).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= stream_timeout - Duration::from_millis(100) && elapsed < REQUEST_TIMEOUT / 2,
            "the stream was reset at its own deadline, not a chunk deadline, but after {elapsed:?}"
        );
    })
    .await
    .expect("the test completes");
}

/// A range response that writes less than the minimum rate over a progress
/// window is abandoned before its whole-stream deadline, even if every chunk
/// is in time: reset with `CANCELLED`, since part of it was written.
///
/// The rate and window are raised and shortened for the test, from
/// [`RANGE_MIN_SERVE_RATE`] over [`RANGE_PROGRESS_WINDOW`].
///
/// [`RANGE_MIN_SERVE_RATE`]: crate::peer::v2::connection::inbound::block_range::RANGE_MIN_SERVE_RATE
/// [`RANGE_PROGRESS_WINDOW`]: crate::peer::v2::connection::inbound::block_range::RANGE_PROGRESS_WINDOW
#[tokio::test]
async fn block_range_responses_below_the_minimum_rate_are_cancelled() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let chain = chain();
    let window = Duration::from_secs(1);

    tokio::time::timeout(TEST_TIMEOUT, async {
        let (requester, responder, _e1, _e2) = connected_pair(&network).await;
        let mut chain_service = ChainService::new(&chain);
        chain_service.per_call = Some(1);
        chain_service.delay = Duration::from_millis(300);
        let (service, _requests) = chain_service.service();
        let _server = spawn_server(
            responder,
            service,
            BlockRangeServing::new(true, global_block_range_permits())
                .with_min_progress(10 * 1024 * 1024, window),
        );

        // One small block every 300 ms is far below 10 MiB/s: the response is abandoned at the
        // first chunk boundary after the window, long before its stream deadline.
        let started = Instant::now();
        let recv = request_range(&requester, tip_range(&chain)).await;
        assert_reset(recv, ErrorCode::Cancelled).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= window && elapsed < REQUEST_TIMEOUT / 2,
            "the stream was reset by the progress rule, but after {elapsed:?}"
        );
    })
    .await
    .expect("the test completes");
}

/// Requests the tip range of `chain` from a server whose reads answer wrongly
/// at `faults`, one block per read, and returns the parsed response, or the
/// reset code if the stream was reset.
async fn tip_range_with_faults(
    chain: &[Arc<Block>],
    faults: impl IntoIterator<Item = (usize, Fault)>,
) -> Result<Parsed, u64> {
    let network = Network::Mainnet;
    let (requester, responder, _e1, _e2) = connected_pair(&network).await;
    let mut chain_service = ChainService::new(chain);
    chain_service.per_call = Some(1);
    chain_service.faults = faults.into_iter().collect();
    let (service, _requests) = chain_service.service();
    let _server = spawn_server(
        responder,
        service,
        BlockRangeServing::new(true, global_block_range_permits()),
    );

    let mut recv = request_range(&requester, tip_range(chain)).await;
    match recv.read_to_end(MAX_TEST_RESPONSE).await {
        Ok(bytes) => Ok(parse(&bytes)),
        Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))) => Err(code.into_inner()),
        Err(error) => panic!("unexpected read error {error:?}"),
    }
}

/// A read that fails, or that the inbound service does not answer with a
/// block range, refuses the stream if it is the first, and finishes the
/// response early with the blocks already written otherwise.
#[tokio::test]
async fn failed_block_range_reads_refuse_or_finish_early() {
    let _init_guard = zebra_test::init();
    let chain = chain();
    let tip = chain.len() - 1;

    tokio::time::timeout(TEST_TIMEOUT, async {
        for fault in [Fault::Error, Fault::Nil] {
            assert_eq!(
                tip_range_with_faults(&chain, [(0, fault)]).await,
                Err(ErrorCode::Refused.code()),
                "{fault:?} on the first read refuses the stream"
            );
            assert_eq!(
                tip_range_with_faults(&chain, [(2, fault)]).await,
                Ok(Parsed::Found(descending(&chain, tip, tip - 1))),
                "{fault:?} on a later read finishes with the blocks already written"
            );
        }
    })
    .await
    .expect("the test completes");
}

/// A block from the inbound service that is not the requested ancestor is
/// never sent: the response is not found if it is the first block, and
/// finishes early with the blocks already written otherwise.
#[tokio::test]
async fn non_chaining_blocks_end_block_range_responses() {
    let _init_guard = zebra_test::init();
    let chain = chain();
    let tip = chain.len() - 1;

    tokio::time::timeout(TEST_TIMEOUT, async {
        assert_eq!(
            tip_range_with_faults(&chain, [(0, Fault::WrongBlock)]).await,
            Ok(Parsed::NotFound),
        );
        assert_eq!(
            tip_range_with_faults(&chain, [(2, Fault::WrongBlock)]).await,
            Ok(Parsed::Found(descending(&chain, tip, tip - 1))),
        );
    })
    .await
    .expect("the test completes");
}
