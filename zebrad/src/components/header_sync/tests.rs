//! Tests for headers-first sync, with fake peers on Regtest.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use chrono::Duration as TimeDelta;
use futures::future::BoxFuture;
use tower::{service_fn, Service, ServiceExt};

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, CountedHeader, Height},
    parameters::{
        testnet::{ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
};
use zebra_network as zn;
use zebra_state as zs;

use super::HeaderSync;
use crate::BoxError;

/// The genesis header, and `count` synthetic Regtest headers above it on fork
/// `fork`, 75 seconds apart. Regtest has a fixed difficulty, no maximum block
/// time, and skips the proof of work, so these pass every header check.
fn chain(count: u32, fork: u8) -> Vec<Arc<block::Header>> {
    let genesis = regtest_genesis_block().header.clone();
    let mut headers = vec![genesis.clone()];

    for height in 1..=count {
        let previous = *headers[headers.len() - 1];
        let mut header = previous;
        header.previous_block_hash = previous.hash();
        header.time = genesis.time + TimeDelta::seconds(75 * i64::from(height));
        header.nonce[0] = fork;
        header.nonce[1..5].copy_from_slice(&height.to_le_bytes());
        headers.push(Arc::new(header));
    }

    headers
}

/// `base` up to `at`, and `count` synthetic headers above it on fork `fork`.
fn fork_of(base: &[Arc<block::Header>], at: u32, count: u32, fork: u8) -> Vec<Arc<block::Header>> {
    let mut headers: Vec<Arc<block::Header>> = base[..=at as usize].to_vec();
    let genesis = base[0].clone();

    for height in at + 1..=at + count {
        let previous = *headers[headers.len() - 1];
        let mut header = previous;
        header.previous_block_hash = previous.hash();
        header.time = genesis.time + TimeDelta::seconds(75 * i64::from(height));
        header.nonce[0] = fork;
        header.nonce[1..5].copy_from_slice(&height.to_le_bytes());
        headers.push(Arc::new(header));
    }

    headers
}

/// Regtest with checkpoints at `heights` of `chain`.
fn regtest(chain: &[Arc<block::Header>], heights: &[u32]) -> Network {
    let checkpoints = heights
        .iter()
        .map(|height| (Height(*height), chain[*height as usize].hash()))
        .collect();

    Network::new_regtest(RegtestParameters {
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(checkpoints)),
        ..Default::default()
    })
}

type State = tower::buffer::Buffer<
    tower::util::BoxService<zs::Request, zs::Response, BoxError>,
    zs::Request,
>;

/// A state with the Regtest genesis block.
async fn state(network: &Network) -> State {
    let (state, _, _, _) = zs::init_test_services(network).await;
    state
        .clone()
        .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
            regtest_genesis_block().into(),
        ))
        .await
        .expect("the genesis block commits");

    state
}

/// Answers `FindHeaders` from `chain` like an honest peer: the headers after
/// the first known hash, up to and including `stop`, at most 160.
fn answer(
    chain: &[Arc<block::Header>],
    known_blocks: &[block::Hash],
    stop: Option<block::Hash>,
) -> Vec<Arc<block::Header>> {
    let hashes: Vec<block::Hash> = chain.iter().map(|header| header.hash()).collect();
    let Some(start) = known_blocks
        .iter()
        .find_map(|known| hashes.iter().position(|hash| hash == known))
    else {
        return chain[1..].iter().take(160).cloned().collect();
    };

    let mut headers = Vec::new();
    for (header, hash) in chain.iter().zip(&hashes).skip(start + 1).take(160) {
        headers.push(header.clone());
        if Some(*hash) == stop {
            break;
        }
    }
    headers
}

type Peers = tower::util::BoxCloneService<zn::Request, zn::Response, BoxError>;

/// A peer set whose peers serve `chain`, and count requests and the most
/// requests in flight at once.
fn honest_peers(
    chain: Vec<Arc<block::Header>>,
    requests: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
) -> Peers {
    let chain = Arc::new(chain);
    let in_flight = Arc::new(AtomicUsize::new(0));

    tower::util::BoxCloneService::new(service_fn(move |request: zn::Request| {
        let chain = chain.clone();
        let requests = requests.clone();
        let in_flight = in_flight.clone();
        let max_in_flight = max_in_flight.clone();

        let future: BoxFuture<'static, Result<zn::Response, BoxError>> = Box::pin(async move {
            let zn::Request::FindHeaders { known_blocks, stop } = request else {
                panic!("headers-first sync only sends FindHeaders");
            };

            requests.fetch_add(1, Ordering::SeqCst);
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);

            Ok(zn::Response::BlockHeaders(
                answer(&chain, &known_blocks, stop)
                    .into_iter()
                    .map(|header| CountedHeader { header })
                    .collect(),
            ))
        });
        future
    }))
}

async fn header_tip(state: &mut State) -> Option<(Height, block::Hash)> {
    match state
        .ready()
        .await
        .expect("the state is ready")
        .call(zs::Request::HeaderTip)
        .await
    {
        Ok(zs::Response::HeaderTip(tip)) => tip,
        response => panic!("unexpected response: {response:?}"),
    }
}

/// Below the last checkpoint, spans download in parallel up to the
/// concurrency limit and are stored in order. Above it, the header chain is
/// extended from a locator until the peer has no more headers.
#[tokio::test(flavor = "multi_thread")]
async fn spans_then_tip_headers_reach_the_peer_tip() {
    let _init_guard = zebra_test::init();

    let headers = chain(45, 0);
    let network = regtest(&headers, &[0, 10, 20, 30, 40]);
    let requests = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let peers = honest_peers(headers.clone(), requests.clone(), max_in_flight.clone());
    let mut state = state(&network).await;

    let mut sync = HeaderSync::new(&network, peers, state.clone(), 2);

    assert_eq!(sync.sync_once().await.expect("spans are valid"), 40);
    assert_eq!(requests.load(Ordering::SeqCst), 4, "one request per span");
    assert!(
        max_in_flight.load(Ordering::SeqCst) <= 2,
        "spans download at most `concurrency` at a time"
    );
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(40), headers[40].hash()))
    );

    assert_eq!(sync.sync_once().await.expect("tip headers are valid"), 5);
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(45), headers[45].hash()))
    );

    assert_eq!(
        sync.sync_once()
            .await
            .expect("the peer has no more headers"),
        0
    );
}

/// A span from a peer that doesn't continue it is dropped without a penalty,
/// and another peer completes it.
#[tokio::test(flavor = "multi_thread")]
async fn headers_that_do_not_continue_a_span_are_dropped() {
    let _init_guard = zebra_test::init();

    let headers = chain(20, 0);
    let other = chain(20, 1);
    let network = regtest(&headers, &[0, 20]);
    let calls = Arc::new(AtomicUsize::new(0));
    let honest = honest_peers(headers.clone(), Arc::default(), Arc::default());

    let peers = tower::util::BoxCloneService::new(service_fn(move |request: zn::Request| {
        let calls = calls.clone();
        let honest = honest.clone();
        let other = other.clone();
        async move {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // Another chain, from its first header: it doesn't connect.
                return Ok(zn::Response::BlockHeaders(
                    other[5..10]
                        .iter()
                        .map(|header| CountedHeader {
                            header: header.clone(),
                        })
                        .collect(),
                ));
            }
            honest.oneshot(request).await
        }
    }));

    let mut state = state(&network).await;
    let mut sync = HeaderSync::new(&network, peers, state.clone(), 1);

    assert_eq!(sync.sync_once().await.expect("the span completes"), 20);
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(20), headers[20].hash()))
    );
}

/// Peers on a chain that doesn't match the checkpoints never get a span
/// stored.
#[tokio::test(flavor = "multi_thread")]
async fn a_span_that_misses_its_checkpoint_is_not_stored() {
    let _init_guard = zebra_test::init();

    let checkpointed = chain(20, 0);
    let network = regtest(&checkpointed, &[0, 10, 20]);
    let other = chain(20, 1);
    let peers = honest_peers(other, Arc::default(), Arc::default());

    let mut state = state(&network).await;
    let mut sync = HeaderSync::new(&network, peers, state.clone(), 2);

    let error = sync
        .sync_once()
        .await
        .expect_err("no peer completes the span at its checkpoint");
    assert!(error.to_string().contains("no peer completed"), "{error}");
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(0), checkpointed[0].hash())),
        "nothing is stored"
    );
}

/// Above the last checkpoint, headers that don't connect to the locator are
/// dropped without a penalty.
#[tokio::test(flavor = "multi_thread")]
async fn tip_headers_that_do_not_connect_are_dropped() {
    let _init_guard = zebra_test::init();

    let headers = chain(10, 0);
    let network = regtest(&headers, &[0]);
    let other = chain(10, 2);

    let peers = tower::util::BoxCloneService::new(service_fn(move |_request: zn::Request| {
        let other = other.clone();
        async move {
            Ok::<_, BoxError>(zn::Response::BlockHeaders(
                other[3..]
                    .iter()
                    .map(|header| CountedHeader {
                        header: header.clone(),
                    })
                    .collect(),
            ))
        }
    }));

    let mut state = state(&network).await;
    let mut sync = HeaderSync::new(&network, peers, state.clone(), 1);

    assert_eq!(sync.sync_once().await.expect("dropping is not an error"), 0);
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(0), headers[0].hash()))
    );
}

/// Above the last checkpoint, a fork deeper than one response, with more work
/// than the stored headers above its fork point, replaces them.
#[tokio::test(flavor = "multi_thread")]
async fn a_fork_deeper_than_one_response_with_more_work_replaces_the_headers() {
    let _init_guard = zebra_test::init();

    let stored = chain(300, 0);
    // Regtest headers all have the same work, so the longer fork has more.
    let fork = fork_of(&stored, 10, 400, 3);
    let network = regtest(&stored, &[0]);
    let mut state = state(&network).await;

    let mut sync = HeaderSync::new(
        &network,
        honest_peers(stored.clone(), Arc::default(), Arc::default()),
        state.clone(),
        1,
    );
    while sync.sync_once().await.expect("the headers are valid") > 0 {}
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(300), stored[300].hash()))
    );

    let mut sync = HeaderSync::new(
        &network,
        honest_peers(fork.clone(), Arc::default(), Arc::default()),
        state.clone(),
        1,
    );
    let added = sync
        .sync_once()
        .await
        .expect("the fork's headers are valid");
    assert!(
        added > 290,
        "the fork replaces the 290 headers above its fork point at once: {added}"
    );
    while sync.sync_once().await.expect("the headers are valid") > 0 {}
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(410), fork[410].hash()))
    );
}

/// Above the last checkpoint, a fork deeper than one response with less work
/// than the stored headers above its fork point doesn't replace them.
#[tokio::test(flavor = "multi_thread")]
async fn a_deep_fork_with_less_work_is_dropped() {
    let _init_guard = zebra_test::init();

    let stored = chain(400, 0);
    let fork = fork_of(&stored, 10, 300, 3);
    let network = regtest(&stored, &[0]);
    let mut state = state(&network).await;

    let mut sync = HeaderSync::new(
        &network,
        honest_peers(stored.clone(), Arc::default(), Arc::default()),
        state.clone(),
        1,
    );
    while sync.sync_once().await.expect("the headers are valid") > 0 {}

    let mut sync = HeaderSync::new(
        &network,
        honest_peers(fork, Arc::default(), Arc::default()),
        state.clone(),
        1,
    );
    assert_eq!(sync.sync_once().await.expect("dropping is not an error"), 0);
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(400), stored[400].hash()))
    );
}

/// A peer that continues a span with headers from another chain can't stop
/// honest peers from completing it: they don't have that chain's last header,
/// so they answer from the span's start, and the span starts again.
#[tokio::test(flavor = "multi_thread")]
async fn a_bogus_partial_span_is_replaced_by_honest_headers() {
    let _init_guard = zebra_test::init();

    let headers = chain(20, 0);
    let bogus = fork_of(&headers, 0, 20, 1);
    let network = regtest(&headers, &[0, 20]);
    let calls = Arc::new(AtomicUsize::new(0));
    let honest = honest_peers(headers.clone(), Arc::default(), Arc::default());

    let peers = tower::util::BoxCloneService::new(service_fn(move |request: zn::Request| {
        let calls = calls.clone();
        let honest = honest.clone();
        let bogus = bogus.clone();
        async move {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // Another chain, from the span's start: it connects, but
                // doesn't reach the checkpoint.
                return Ok(zn::Response::BlockHeaders(
                    bogus[1..6]
                        .iter()
                        .map(|header| CountedHeader {
                            header: header.clone(),
                        })
                        .collect(),
                ));
            }
            honest.oneshot(request).await
        }
    }));

    let mut state = state(&network).await;
    let mut sync = HeaderSync::new(&network, peers, state.clone(), 1);

    assert_eq!(sync.sync_once().await.expect("the span completes"), 20);
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(20), headers[20].hash()))
    );
}

/// A span longer than the requests one attempt has on Mainnet, between
/// checkpoints further apart than Mainnet's, still completes.
#[tokio::test(flavor = "multi_thread")]
async fn a_span_longer_than_the_request_budget_completes() {
    let _init_guard = zebra_test::init();

    let length = u32::try_from(super::MAX_SPAN_REQUESTS * super::MAX_HEADERS_PER_RESPONSE)
        .expect("the test span fits in a u32")
        + 100;
    let headers = chain(length, 0);
    let network = regtest(&headers, &[0, length]);
    let requests = Arc::new(AtomicUsize::new(0));
    let peers = honest_peers(
        headers.clone(),
        requests.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    let mut state = state(&network).await;

    let mut sync = HeaderSync::new(&network, peers, state.clone(), 2);

    assert_eq!(
        sync.sync_once().await.expect("the span is valid"),
        usize::try_from(length).expect("the test span fits in a usize")
    );
    assert!(requests.load(Ordering::SeqCst) > super::MAX_SPAN_REQUESTS);
    assert_eq!(
        header_tip(&mut state).await,
        Some((Height(length), headers[length as usize].hash()))
    );
}

/// An invalid reply that links to a span's start, or a reply that runs past the
/// span's checkpoint, doesn't erase the span's verified progress: invalid replies
/// alternating with honest ones still let the span complete.
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_reply_from_the_span_start_keeps_progress() {
    let _init_guard = zebra_test::init();

    let headers = chain(400, 0);
    // The same chain, longer than the span, for replies that run past its checkpoint.
    let longer = chain(1000, 0);
    let network = regtest(&headers, &[0, 400]);
    let calls = Arc::new(AtomicUsize::new(0));
    let honest = honest_peers(headers.clone(), Arc::default(), Arc::default());

    let peers = tower::util::BoxCloneService::new(service_fn(move |request: zn::Request| {
        let calls = calls.clone();
        let honest = honest.clone();
        let headers = headers.clone();
        let longer = longer.clone();
        async move {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            let zn::Request::FindHeaders { known_blocks, .. } = &request else {
                panic!("headers-first sync only sends FindHeaders");
            };
            let reply = match call % 4 {
                // A header that links to the span's start, but is too far in the future.
                1 => {
                    let mut header = *headers[1];
                    header.time = chrono::Utc::now() + TimeDelta::hours(3);
                    vec![Arc::new(header)]
                }
                // Headers that continue the span, but run past its checkpoint.
                3 => {
                    let start = longer
                        .iter()
                        .position(|header| header.hash() == known_blocks[0])
                        .expect("the requester's last header is on the chain");
                    longer[start + 1..].iter().take(401).cloned().collect()
                }
                _ => return honest.oneshot(request).await,
            };
            Ok(zn::Response::BlockHeaders(
                reply
                    .into_iter()
                    .map(|header| CountedHeader { header })
                    .collect(),
            ))
        }
    }));

    let mut state = state(&network).await;
    let mut sync = HeaderSync::new(&network, peers, state.clone(), 1);

    assert_eq!(sync.sync_once().await.expect("the span completes"), 400);
    assert_eq!(
        header_tip(&mut state).await.map(|tip| tip.0),
        Some(Height(400))
    );
}
