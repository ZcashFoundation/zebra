//! Fixed test vectors for the inbound gossip block download stream.
//!
//! These are the inbound-gossip siblings of the sync-path tests in
//! `crate::components::sync::downloads::tests`: they cover GHSA-4f6v-mj46-gxg3, where a peer
//! answers a `BlockDownload` request with a canonical header and a rewritten coinbase height. The
//! coinbase scriptSig is excluded from the V5 transaction ID (ZIP-244), so the block hash is
//! unchanged and the response passes the hash check, but the forged height is dropped as too far
//! ahead of the tip, or behind the finalized tip, before consensus validation. The supplying peer
//! must be scored only when the parent header Zebra holds proves the height was rewritten.

use std::{sync::Arc, time::Duration};

use futures::stream::StreamExt;

use zebra_chain::{
    block::{Block, Height},
    chain_tip::mock::MockChainTip,
    serialization::ZcashDeserializeInto,
};
use zebra_network::{InventoryResponse, PeerSocketAddr};
use zebra_state::MAX_BLOCK_REORG_HEIGHT;
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use super::{DownloadAction, Downloads, HeightLimitError, MIN_CONCURRENCY_LIMIT};

use InventoryResponse::*;

/// Maximum time to wait for a request to any test service.
///
/// These tests use only in-process mocks and pause Tokio time, preserving the full no-request
/// observation window without spending that time idle.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// A peer address used to check whether the supplying peer can be scored.
const ADVERTISER: &str = "127.0.0.1:8233";

type MockNetwork = MockService<zn::Request, zn::Response, PanicAssertion>;
type MockVerifier = MockService<zebra_consensus::Request, zebra_chain::block::Hash, PanicAssertion>;
type MockState = MockService<zs::Request, zs::Response, PanicAssertion>;

/// Build an inbound gossip downloader whose state tip is at `tip_height`, returning the mock
/// network, verifier, and state services so a test can drive and assert on them.
#[allow(clippy::type_complexity)]
fn mock_downloads(
    tip_height: Height,
) -> (
    Downloads<MockNetwork, MockVerifier, MockState, MockChainTip>,
    MockNetwork,
    MockVerifier,
    MockState,
) {
    let network: MockNetwork = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let verifier: MockVerifier = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let state: MockState = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let (latest_chain_tip, chain_tip_sender) = MockChainTip::new();
    chain_tip_sender.send_best_tip_height(tip_height);
    // Dropping the sender is fine: `watch::Receiver::borrow` keeps returning the height we just
    // sent, and no test changes the tip mid-run.

    let downloads = Downloads::new(
        MIN_CONCURRENCY_LIMIT,
        network.clone(),
        verifier.clone(),
        state.clone(),
        latest_chain_tip,
        zebra_chain::parameters::Network::Mainnet,
    );

    (downloads, network, verifier, state)
}

/// Overlapping sync and inbound holders keep provenance for their exact bodies and release only
/// their own record, even if a mock reuses the same Arc for different supplying peers.
#[test]
fn concurrent_download_sources_follow_exact_bodies_and_guard_lifetimes() {
    let _init_guard = zebra_test::init();
    let registry = crate::components::sync::InFlightDownloads::default();
    let first_body = block_1();
    let second_body = Arc::new((*first_body).clone());
    let hash = first_body.hash();
    let first_peer = ADVERTISER.parse().unwrap();
    let second_peer = "127.0.0.2:8233".parse().unwrap();
    let third_peer = "127.0.0.3:8233".parse().unwrap();
    let mut inbound = registry.claim(hash).unwrap();
    inbound.set_source(&first_body, Some(first_peer));
    let mut sync = registry.hold(hash);
    sync.set_source(&second_body, Some(second_peer));
    let mut replacement = registry.hold(hash);
    replacement.set_source(&second_body, Some(third_peer));
    assert_eq!(registry.source(&hash, &first_body), Some(first_peer));
    assert_eq!(registry.source(&hash, &second_body), Some(second_peer));
    drop(sync);
    assert_eq!(registry.source(&hash, &first_body), Some(first_peer));
    assert_eq!(registry.source(&hash, &second_body), Some(third_peer));
    drop(inbound);
    assert_eq!(registry.source(&hash, &first_body), None);
    assert_eq!(registry.source(&hash, &second_body), Some(third_peer));
    assert!(
        registry.contains(&hash),
        "the replacement owner is still alive"
    );
    drop(replacement);
    assert!(!registry.contains(&hash));
}

/// Locally authenticated bodies reject echoed inventory only while their verifier is alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn early_relay_cache_skips_echo_download_until_withdrawn() {
    use tower::{Service, ServiceExt};
    use zebra_chain::{
        block::{self, ChainHistoryBlockTxAuthCommitmentHash},
        parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
        transaction::{LockTime, Transaction},
        transparent,
    };

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let (mut state, read, _, _) =
            zs::init(zs::Config::ephemeral(), &network, Height(0), 0).await;
        let genesis = block::genesis::regtest_genesis_block();
        state
            .ready()
            .await
            .unwrap()
            .call(zs::Request::CommitCheckpointVerifiedBlock(
                genesis.clone().into(),
            ))
            .await
            .unwrap();
        let zs::ReadResponse::EarlyRelayCache(cache) = read
            .clone()
            .oneshot(zs::ReadRequest::EarlyRelayCache)
            .await
            .unwrap()
        else {
            panic!("requested the early relay cache");
        };
        let mut candidate = genesis;
        for height in [Height(1), Height(2)] {
            let zs::ReadResponse::ChainInfo(info) = read
                .clone()
                .oneshot(zs::ReadRequest::ChainInfo)
                .await
                .unwrap()
            else {
                panic!("requested chain info");
            };
            let transactions = vec![Arc::new(Transaction::test_v5(
                NetworkUpgrade::Nu5,
                vec![transparent::Input::Coinbase {
                    height,
                    data: vec![0],
                    sequence: u32::MAX,
                }],
                vec![transparent::Output::new(
                    zebra_chain::parameters::subsidy::block_subsidy(height, &network).unwrap(),
                    transparent::Script::new(&[0x51]),
                )],
                LockTime::unlocked(),
                height,
            ))];
            let mut block = Block {
                header: Arc::new(block::Header {
                    version: 4,
                    previous_block_hash: info.tip_hash,
                    merkle_root: transactions.iter().map(|tx| tx.hash()).collect(),
                    commitment_bytes: [0; 32].into(),
                    time: info.cur_time.into(),
                    difficulty_threshold: info.expected_difficulty,
                    nonce: [0; 32].into(),
                    solution: zebra_chain::work::equihash::Solution::Regtest([0; 36]),
                }),
                transactions,
            };
            Arc::make_mut(&mut block.header).commitment_bytes =
                <[u8; 32]>::from(ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                    &info
                        .chain_history_root
                        .unwrap_or(block::CHAIN_HISTORY_ACTIVATION_RESERVED.into()),
                    &block.auth_data_root(),
                ))
                .into();
            candidate = Arc::new(block);
            if height == Height(1) {
                state
                    .ready()
                    .await
                    .unwrap()
                    .call(zs::Request::CommitSemanticallyVerifiedBlock(
                        zs::CheckpointVerifiedBlock::from(candidate.clone()).into(),
                    ))
                    .await
                    .unwrap();
            }
        }
        let admission = state.ready().await.unwrap().call(zs::Request::EarlyRelay {
            block: candidate.clone(),
            require_authentication: true,
        });
        // State commits need a multithreaded runtime's blocking sections, but admission and
        // every cache lifetime assertion run on a separate paused clock, independent of host load.
        tokio::task::spawn_blocking(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .start_paused(true)
                .build()
                .unwrap()
                .block_on(async move {
                    let admission = admission.await.unwrap();
        assert!(matches!(admission, zs::Response::EarlyRelay(Some(_))));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(cache.get(&candidate.hash()).is_some());
        let (downloads, _network, _verifier, mut state) = mock_downloads(Height(1));
        let mut downloads = downloads.with_cached_blocks(Default::default(), Some(cache.clone()));
        let peer = ADVERTISER.parse().unwrap();
        let in_flight = crate::components::sync::InFlightDownloads::default();
        let mut ownership = in_flight.claim(candidate.hash()).unwrap();
        ownership.set_source(&candidate, Some(peer));
        let competing_body = Arc::new((*candidate).clone());
        let mut competing = in_flight.hold(candidate.hash());
        competing.set_source(&competing_body, Some("127.0.0.2:8233".parse().unwrap()));
        let mut gossip_network: MockNetwork = MockService::build().for_unit_tests();
        {
            let gossip = crate::components::sync::gossip_early_blocks(
                Some(cache.clone()),
                gossip_network.clone(),
                in_flight.clone(),
            );
            tokio::pin!(gossip);
            // Keep polling the producer when watch/readiness yields to Tokio's cooperative budget.
            tokio::select! {
                biased;
                result = &mut gossip => panic!("gossip must remain active: {result:?}"),
                request = gossip_network.expect_request(zn::Request::AdvertiseEarlyBlock(
                    candidate.hash(),
                    Some(peer),
                )) => {
                    request.respond(zn::Response::Nil);
                }
            }
        }
        drop(ownership);
        assert!(cache.get(&candidate.hash()).is_some());
        assert_eq!(
            in_flight.early_relay_source(&candidate.hash(), &cache),
            None,
            "a surviving verifier must not turn a canceled download into a local submission",
        );
        assert!(matches!(
            downloads.download_and_verify(candidate.hash(), Some(peer)),
            DownloadAction::AlreadyQueued,
        ));
        assert!(
            downloads.pending.is_empty(),
            "cached bodies must not spawn a download",
        );
        drop(admission);
        assert!(cache.get(&candidate.hash()).is_none());
        assert!(matches!(
            downloads.download_and_verify(candidate.hash(), Some(peer)),
            DownloadAction::AddedToQueue,
        ));
        state
            .expect_request(zs::Request::KnownBlock(candidate.hash()))
            .await
            .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));
        assert!(downloads.next().await.unwrap().is_err());
                })
        })
        .await
        .expect("paused cache regression must not panic");
    })
    .await
    .expect("cache admission and withdrawal complete within the deadline");
}

/// Negotiated downloads preserve both the verification contract and the actual serving address.
#[tokio::test]
async fn early_relay_download_preserves_validation_and_source() {
    let _init_guard = zebra_test::init();
    for (early, admitted) in [(false, false), (true, false), (true, true)] {
        let (mut downloads, mut network, mut verifier, mut state) = mock_downloads(Height(0));
        let in_flight = crate::components::sync::InFlightDownloads::default();
        downloads = downloads.with_sync_downloads(in_flight.clone());
        let block = block_1();
        let hash = block.hash();
        let peer: PeerSocketAddr = ADVERTISER.parse().unwrap();
        let supplier: PeerSocketAddr = "127.0.0.2:8233".parse().unwrap();
        assert!(matches!(
            downloads.download_and_verify(hash, Some(peer)),
            DownloadAction::AddedToQueue
        ));
        state
            .expect_request(zs::Request::KnownBlock(hash))
            .await
            .respond(zs::Response::KnownBlock(None));
        let blocks = vec![Available((block.clone(), Some(supplier)))];
        network
            .expect_request(zn::Request::BlockDownload {
                hash,
                advertiser: Some(peer),
            })
            .await
            .respond(if early {
                zn::Response::EarlyBlocks(blocks)
            } else {
                zn::Response::Blocks(blocks)
            });
        state
            .expect_request(zs::Request::KnownBlock(block.header.previous_block_hash))
            .await
            .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));
        let request = if early {
            zebra_consensus::Request::CommitEarlyRelay(block.clone())
        } else {
            zebra_consensus::Request::Commit(block.clone())
        };
        let error = zebra_consensus::VerifyBlockError::Transaction(
            zebra_consensus::error::TransactionError::Groth16("invalid proof".into()),
        );
        let error = if admitted {
            zebra_consensus::VerifyBlockError::EarlyRelay(Box::new(error))
        } else {
            error
        };
        let verification = verifier.expect_request(request).await;
        assert_eq!(
            in_flight.source(&hash, &block),
            Some(supplier),
            "early fanout must exclude the actual supplier, not merely the announcing peer",
        );
        verification.respond_error(zebra_consensus::RouterError::from(error).into());
        let (error, source) = downloads.next().await.unwrap().unwrap_err();
        assert_eq!(source, Some(supplier));
        assert_eq!(
            in_flight.source(&hash, &block),
            None,
            "completion releases source attribution",
        );
        assert_eq!(
            error
                .downcast_ref::<zebra_consensus::RouterError>()
                .unwrap()
                .misbehavior_score(),
            if admitted { 0 } else { 100 },
            "only authenticated early-relay transaction failures are non-punitive",
        );
    }
}

/// Load the mainnet block 1 vector, which these tests use as a low-height block body.
fn block_1() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// Load the mainnet block 2 vector.
fn block_2() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// Load the mainnet genesis block vector.
fn genesis() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// Load the mainnet block 10 vector, which the far-ahead tests use as a high-height block body.
fn block_10() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_10_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// The tip height that puts block 2 exactly on the lookahead limit.
///
/// The downloader accepts blocks up to `MIN_CONCURRENCY_LIMIT` above the tip, so block 10 is far
/// ahead of this tip, and block 2 is the highest block it still verifies.
fn lookahead_boundary_tip() -> Height {
    Height(2 - u32::try_from(MIN_CONCURRENCY_LIMIT).expect("small constant fits in u32"))
}

/// Regression test for GHSA-4f6v-mj46-gxg3: a gossiped body whose claimed height contradicts the
/// parent we already hold is attributed to the peer that supplied it, and never reaches consensus.
///
/// A peer can answer with a canonical header and rewritten coinbase height because the initial hash
/// check does not recompute the header's commitment to the body's authorizing data. The parent is
/// the proof: a block's height is one more than its parent's.
#[tokio::test(start_paused = true)]
async fn contradicted_behind_tip_height_is_attributed_and_never_verified() {
    let _init_guard = zebra_test::init();

    // The tip is far enough ahead that a claimed height of 1 is behind the reorg limit.
    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    // Block 2's header with block 1's body: the hash and parent are block 2's, because the hash
    // covers only the header, but the coinbase now claims height 1 instead of 2. This is the shape
    // of the reported attack, without reproducing the hash-preserving rewrite itself.
    let block_1 = block_1();
    let block_2 = block_2();
    let block = Arc::new(Block {
        header: block_2.header.clone(),
        transactions: block_1.transactions.clone(),
    });
    let hash = block.hash();
    assert_eq!(
        hash,
        block_2.hash(),
        "the block hash covers only the header"
    );
    assert_eq!(
        block.coinbase_height(),
        Some(Height(1)),
        "the body must claim the height the downloader reads"
    );

    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    // The block is not already in the state.
    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // We hold the real parent, block 1, so the body's real height is 2, not the 1 it claims.
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::Response::BlockHeader {
            header: block_1.header.clone(),
            hash: block_1.hash(),
            height: Height(1),
            next_block_hash: Some(hash),
        });

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "a contradicted behind-tip height must be a scoreable typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr,
        Some(advertiser),
        "a contradicted behind-tip height must attribute the drop to the supplying peer"
    );

    // The rewritten body is dropped before consensus validation, which is why the peer must be
    // scored on this path instead.
    verifier.expect_no_requests().await;
}

/// A peer that serves a genuinely old block is not attributed, so it cannot be scored.
///
/// Its height agrees with its parent's, so there is no proof of misbehaviour and the drop must stay
/// anonymous.
#[tokio::test(start_paused = true)]
async fn genuinely_old_block_is_dropped_without_attribution() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The parent is genesis, one below the height the body claims: the body is authentic.
    let genesis = genesis();
    assert_eq!(
        block.header.previous_block_hash,
        genesis.hash(),
        "block 1's parent is genesis"
    );
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::Response::BlockHeader {
            header: genesis.header.clone(),
            hash: genesis.hash(),
            height: Height(0),
            next_block_hash: Some(hash),
        });

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "an old block must still be a typed behind-tip error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "an authentic old block must not be attributed to its peer"
    );

    verifier.expect_no_requests().await;
}

/// A peer whose old block has a parent we do not hold is not attributed either: without the parent
/// there is no proof the height was rewritten.
#[tokio::test(start_paused = true)]
async fn behind_tip_block_with_unknown_parent_is_not_attributed() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(Err(zn::BoxError::from("block not found in any chain")));

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "a behind-tip drop must be a typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "a block whose parent we do not hold must not be attributed"
    );

    verifier.expect_no_requests().await;
}

/// The parent lookup is bounded: when the state does not answer, the block is still dropped and the
/// peer is still not attributed.
///
/// Paused time lets the runtime advance past `PARENT_LOOKUP_TIMEOUT` as soon as the download task is
/// the only thing waiting, so the test does not sleep for real.
#[tokio::test(start_paused = true)]
async fn behind_tip_parent_lookup_timeout_is_not_attributed() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The parent lookup is deliberately never answered, so it can only end by timing out.
    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "a timed-out lookup must still drop with a typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "a timed-out parent lookup must not attribute the drop"
    );

    verifier.expect_no_requests().await;
}

/// A block at the oldest height that is still within the reorg limit is verified, not dropped.
///
/// This is the guard against dropping and attributing honest near-boundary blocks:
/// `min_accepted_height` is the boundary, and the behind-tip comparison is strict, so a block
/// exactly on it is a normal download.
#[tokio::test(start_paused = true)]
async fn block_at_reorg_boundary_is_verified_not_dropped() {
    let _init_guard = zebra_test::init();

    // `min_accepted_height` is `Height(1)`, so the height-1 block sits exactly on the boundary.
    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(MAX_BLOCK_REORG_HEIGHT + 1));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    state
        .expect_request(zs::Request::KnownBlock(block.header.previous_block_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));
    verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(hash);

    assert_eq!(
        downloads
            .next()
            .await
            .expect("downloads is non-empty")
            .expect("block on the reorg boundary is verified"),
        hash,
        "a block at min_accepted_height must be verified, not dropped as behind the tip"
    );

    // The parent is known, so no recovery or height-contradiction lookup is needed.
    state.expect_no_requests().await;
}

/// The far-ahead sibling of the regression test for GHSA-4f6v-mj46-gxg3: a gossiped body whose
/// claimed height is above the lookahead limit, and contradicts the parent we already hold, is
/// attributed to the peer that supplied it, and never reaches consensus.
///
/// The sync path deliberately leaves its far-ahead drops unscored (GHSA-qhr3-cvch-5fh2), because
/// the serving peer did not choose the height of a genuine block. Here the parent is the proof, so
/// scoring is safe: a genuine block whose parent we hold is at most one above the tip, so its real
/// height is never far ahead.
#[tokio::test(start_paused = true)]
async fn contradicted_far_ahead_height_is_attributed_and_never_verified() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(lookahead_boundary_tip());

    // Block 2's header with block 10's body: the hash and parent are block 2's, because the hash
    // covers only the header, but the coinbase now claims height 10 instead of 2.
    let block_2 = block_2();
    let block_10 = block_10();
    let block = Arc::new(Block {
        header: block_2.header.clone(),
        transactions: block_10.transactions.clone(),
    });
    let hash = block.hash();
    assert_eq!(
        hash,
        block_2.hash(),
        "the block hash covers only the header"
    );
    assert_eq!(
        block.coinbase_height(),
        Some(Height(10)),
        "the body must claim the height the downloader reads"
    );

    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // We hold the real parent, block 1, so the body's real height is 2, not the 10 it claims.
    let block_1 = block_1();
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::Response::BlockHeader {
            header: block_1.header.clone(),
            hash: block_1.hash(),
            height: Height(1),
            next_block_hash: Some(hash),
        });

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block above the lookahead limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::AboveLookahead { .. })
        ),
        "a contradicted far-ahead height must be a scoreable typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr,
        Some(advertiser),
        "a contradicted far-ahead height must attribute the drop to the supplying peer"
    );

    verifier.expect_no_requests().await;
}

/// A peer that serves a genuinely far-ahead block is not attributed, so it cannot be scored.
///
/// This is the common case while Zebra is catching up: the block's parent is not in the state yet,
/// so there is no proof of misbehaviour and the drop must stay anonymous.
#[tokio::test(start_paused = true)]
async fn genuinely_far_ahead_block_is_dropped_without_attribution() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(lookahead_boundary_tip());

    let block = block_10();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The parent, block 9, is far ahead too, so we do not hold it.
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(Err(zn::BoxError::from("block hash or height not found")));

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block above the lookahead limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::AboveLookahead { .. })
        ),
        "a far-ahead drop must be a typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "a genuinely far-ahead block must not be attributed to its peer"
    );

    verifier.expect_no_requests().await;
}

/// A block at the highest height that is still within the lookahead limit is verified, not dropped.
///
/// This is the guard against dropping and attributing honest near-boundary blocks:
/// `max_lookahead_height` is the boundary, and the far-ahead comparison is strict, so a block exactly
/// on it is a normal download.
#[tokio::test(start_paused = true)]
async fn block_at_lookahead_boundary_is_verified_not_dropped() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(lookahead_boundary_tip());

    let block = block_2();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: Some(advertiser),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    state
        .expect_request(zs::Request::KnownBlock(block.header.previous_block_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));
    verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(hash);

    assert_eq!(
        downloads
            .next()
            .await
            .expect("downloads is non-empty")
            .expect("block on the lookahead boundary is verified"),
        hash,
        "a block at max_lookahead_height must be verified, not dropped as far ahead"
    );

    // The parent is known, so no recovery or height-contradiction lookup is needed.
    state.expect_no_requests().await;
}

mod missing_parent;
mod network_checkpoint;

/// A gossip download cannot wait indefinitely for readiness or reset its budget at dispatch.
#[tokio::test(start_paused = true)]
async fn download_deadline_includes_readiness_and_response() {
    use tokio::time::{sleep, timeout_at, Instant};
    use tower::{limit::ConcurrencyLimit, ServiceExt};

    let _init_guard = zebra_test::init();
    let budget = zn::constants::REQUEST_TIMEOUT + super::BLOCK_DOWNLOAD_TIMEOUT;
    for release_capacity in [false, true] {
        let mut network: MockNetwork = MockService::build().for_unit_tests();
        let limited = ConcurrencyLimit::new(network.clone(), 1);
        let mut reservation = limited.clone();
        reservation.ready().await.unwrap();
        let verifier: MockVerifier = MockService::build().for_unit_tests();
        let mut state: MockState = MockService::build().for_unit_tests();
        let (tip, _sender) = MockChainTip::new();
        let mut downloads = Downloads::new(
            2,
            limited,
            verifier,
            state.clone(),
            tip,
            zebra_chain::parameters::Network::Mainnet,
        );
        let hash = block_1().hash();
        assert!(matches!(
            downloads.download_and_verify(hash, None),
            DownloadAction::AddedToQueue,
        ));
        state
            .expect_request(zs::Request::KnownBlock(hash))
            .await
            .respond(zs::Response::KnownBlock(None));
        let started = Instant::now();
        tokio::task::yield_now().await;
        let _response = if release_capacity {
            sleep(budget / 2).await;
            drop(reservation);
            Some(
                network
                    .expect_request(zn::Request::BlockDownload {
                        hash,
                        advertiser: None,
                    })
                    .await,
            )
        } else {
            None
        };
        let (error, advertiser) =
            timeout_at(started + budget + Duration::from_secs(1), downloads.next())
                .await
                .expect("gossip readiness and response must be bounded")
                .expect("the queued download has a result")
                .expect_err("the unanswered download expires");
        assert!(error.is::<tokio::time::error::Elapsed>());
        assert_eq!(advertiser, None);
        assert_eq!(Instant::now() - started, budget);
    }
}
