//! Fixed test vectors for the syncer's block download stream.

use std::{net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use futures::{stream::StreamExt, FutureExt};
use proptest::{
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};
use tokio::{
    sync::{oneshot, watch},
    time::timeout,
};
use tower::{buffer::Buffer, builder::ServiceBuilder, util::BoxService, Service, ServiceExt};
use tracing::Span;

use zebra_chain::{
    block::{self, arbitrary::allow_all_transparent_coinbase_spends, Block, Height},
    chain_tip::{mock::MockChainTip, ChainTip},
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, ParametersBuilder},
        NetworkUpgrade,
    },
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction as chain_transaction,
    work::difficulty::U256,
    LedgerState,
};
use zebra_network::{
    constants::DEFAULT_MAX_CONNS_PER_IP, AddressBook, InventoryResponse, PeerSocketAddr,
};
use zebra_node_services::mempool;
use zebra_state::{Config as StateConfig, MAX_BLOCK_REORG_HEIGHT};
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::components::{
    inbound::{downloads::MAX_INBOUND_CONCURRENCY, Inbound, InboundSetupData},
    sync::{
        ChainSync, BLOCK_DOWNLOAD_RETRY_LIMIT, MIN_CHECKPOINT_CONCURRENCY_LIMIT, SYNC_RESTART_DELAY,
    },
};

use super::{BlockDownloadVerifyError, Downloads};

use InventoryResponse::*;

/// Maximum time to wait for a request to any test service.
///
/// These tests run the download task in parallel with the test, so machines under heavy load need a
/// longer delay.
///
/// Mock-only tests pause Tokio time to retain this window without idle wall time.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// Upper bound on a real state commit finishing, for tests that use a real state and verifier.
/// It only caps failures, so it is generous for slow CI runners.
const MAX_STATE_COMMIT_DELAY: Duration = Duration::from_secs(10);

/// A peer address used to check whether the supplying peer can be scored.
const ADVERTISER: &str = "127.0.0.1:8233";

type MockNetwork = MockService<zn::Request, zn::Response, PanicAssertion>;
type MockVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;
type MockState = MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>;

/// Returns a copy of `block` with one byte of its coinbase miner data flipped.
///
/// ZIP-244 excludes scriptSigs from v5 transaction IDs, so the block hash is unchanged,
/// but the authorizing data commitment is not.
fn coinbase_authorization_twin(block: &Arc<Block>) -> Arc<Block> {
    let mut coinbase = block.transactions[0]
        .zcash_serialize_to_vec()
        .expect("coinbase serializes");

    // A v5 header is 20 bytes, followed by the input count and the 36-byte null prevout.
    const SCRIPT_LEN_OFFSET: usize = 20 + 1 + 36;
    assert_eq!(
        coinbase[SCRIPT_LEN_OFFSET - 4..SCRIPT_LEN_OFFSET],
        [0xff; 4]
    );
    let script_end = SCRIPT_LEN_OFFSET + 1 + usize::from(coinbase[SCRIPT_LEN_OFFSET]);
    coinbase[script_end - 1] ^= 0x01;

    let coinbase: chain_transaction::Transaction = coinbase
        .zcash_deserialize_into()
        .expect("mutated coinbase parses");

    let mut twin = block.clone();
    Arc::make_mut(&mut twin).transactions[0] = Arc::new(coinbase);
    twin
}

/// Checks that a checkpoint block whose body is replaced by a same-hash authorization twin
/// (served through the inbound downloader while the syncer's copy is queued) is downloadable
/// again after the state rejects the twin, and that the honest block then commits without a
/// restart.
///
/// Regression test for GHSA-9hq5-pfgc-cx5h.
#[tokio::test(flavor = "multi_thread")]
async fn rejected_checkpoint_twin_is_downloaded_again() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    // Keep this early-upgrade schedule within the monetary cap.
    let network_builder = ParametersBuilder::default()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            before_overwinter: Some(1),
            overwinter: Some(2),
            sapling: Some(3),
            blossom: Some(4),
            heartwood: Some(5),
            canopy: Some(6),
            nu5: Some(7),
            nu6: None,
            nu6_1: None,
            nu6_2: None,
            nu6_3: None,
            nu7: None,
        })
        .expect("valid activation heights")
        .with_target_difficulty_limit(U256::MAX)
        .expect("valid permissive test difficulty limit")
        .with_disable_pow(true)
        .extend_funding_streams()
        .expect("valid funding streams");
    let generation_network = network_builder
        .clone()
        .to_network()
        .expect("valid generation network");

    let mut runner = TestRunner::deterministic();
    let ledger =
        LedgerState::genesis_strategy(Some(generation_network), NetworkUpgrade::Nu5, Some(5), true)
            .new_tree(&mut runner)
            .expect("ledger strategy generates")
            .current();
    let chain =
        Block::partial_chain_strategy(ledger, 10, allow_all_transparent_coinbase_spends, true)
            .new_tree(&mut runner)
            .expect("chain strategy generates")
            .current()
            .0;

    let target_index = chain
        .iter()
        .position(|block| block.coinbase_height() == Some(Height(7)))
        .expect("generated chain reaches NU5");
    let honest = chain[target_index].clone();
    let child = chain[target_index + 1].clone();
    let twin = coinbase_authorization_twin(&honest);
    assert_eq!(twin.hash(), honest.hash());
    assert_eq!(twin.transactions[0].hash(), honest.transactions[0].hash());
    assert_ne!(twin.auth_data_root(), honest.auth_data_root());

    let network = network_builder
        .with_genesis_hash(chain[0].hash())
        .expect("generated genesis hash is valid")
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (Height(0), chain[0].hash()),
            (Height(8), child.hash()),
        ]))
        .expect("valid generated checkpoint list")
        .to_network()
        .expect("valid final test network");

    let (state, read_state, latest_chain_tip, _chain_tip_change) =
        zebra_state::init(StateConfig::ephemeral(), &network, Height(8), 1_000).await;
    let mut state_service = ServiceBuilder::new().buffer(100).service(state);
    for prefix in chain.iter().take(target_index) {
        state_service
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                prefix.clone().into(),
            ))
            .await
            .expect("valid prefix checkpoint commits");
    }

    let (block_verifier, _tx_verifier, consensus_handles, max_checkpoint_height, _prepared_blocks) =
        zebra_consensus::router::init_test(
            zebra_consensus::Config::default(),
            &network,
            state_service.clone(),
            read_state.clone(),
        )
        .await;
    assert_eq!(max_checkpoint_height, Height(8));

    let peer_set = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let mut peer_set_handle = peer_set.clone();
    let buffered_peer_set = Buffer::new(BoxService::new(peer_set), 10);

    let (past_lookahead_limit_sender, _past_lookahead_limit_receiver) = watch::channel(false);
    let mut sync_downloads = Downloads::new(
        buffered_peer_set.clone(),
        block_verifier.clone(),
        read_state,
        latest_chain_tip.clone(),
        past_lookahead_limit_sender,
        1_000,
        Height(8),
    );

    let mempool_service: MockService<mempool::Request, mempool::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let buffered_mempool = Buffer::new(BoxService::new(mempool_service), 10);
    let address_book = Arc::new(std::sync::Mutex::new(AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        &network,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    )));
    let (setup_tx, setup_rx) = oneshot::channel();
    let (misbehavior_sender, mut misbehavior_rx) = tokio::sync::mpsc::channel(10);
    let mut inbound = Inbound::new(MAX_INBOUND_CONCURRENCY, setup_rx);
    setup_tx
        .send(InboundSetupData {
            address_book,
            block_download_peer_set: buffered_peer_set,
            block_verifier,
            mempool: buffered_mempool,
            state: state_service.clone(),
            latest_chain_tip: latest_chain_tip.clone(),
            misbehavior_sender,
        })
        .map_err(|_| "inbound setup receiver remains open")?;
    inbound.ready().await?;

    // The production sync downloader owns the first request and keeps it pending in the shared
    // checkpoint verifier after the canonical body is downloaded.
    sync_downloads
        .download_and_verify(honest.hash())
        .await
        .expect("sync download is queued");
    let sync_request = peer_set_handle
        .expect_request(zn::Request::BlockDownload {
            hash: honest.hash(),
            advertiser: None,
        })
        .await;
    let attacker_h: PeerSocketAddr = "192.168.180.28:10000".parse().unwrap();
    sync_request.respond(zn::Response::Blocks(vec![Available((
        honest.clone(),
        Some(attacker_h),
    ))]));
    // The honest body must reach the checkpoint verifier before the twin, so the twin is the
    // newer request that replaces it. Give the spawned download task time to run.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The same hash is absent from state, so Inbound's separate downloader accepts the later
    // advertisement and delivers the authorization twin to the same verifier.
    assert_eq!(
        inbound
            .ready()
            .await?
            .call(zn::Request::AdvertiseBlock(honest.hash(), Some(attacker_h),))
            .await?,
        zn::Response::Nil
    );
    peer_set_handle
        .expect_request(zn::Request::BlockDownload {
            hash: honest.hash(),
            advertiser: Some(attacker_h),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            twin.clone(),
            Some(attacker_h),
        ))]));
    tokio::time::sleep(Duration::from_millis(25)).await;

    // A different attacker IP provides the withheld range-completing body immediately after the
    // twin is queued, satisfying the inbound one-download-per-IP cap.
    let attacker_b: PeerSocketAddr = "192.168.180.29:10001".parse().unwrap();
    assert_eq!(
        inbound
            .ready()
            .await?
            .call(zn::Request::AdvertiseBlock(child.hash(), Some(attacker_b),))
            .await?,
        zn::Response::Nil
    );
    peer_set_handle
        .expect_request(zn::Request::BlockDownload {
            hash: child.hash(),
            advertiser: Some(attacker_b),
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            child.clone(),
            Some(attacker_b),
        ))]));

    let sync_error = timeout(MAX_STATE_COMMIT_DELAY, sync_downloads.next())
        .await
        .expect("replaced sync request resolves")
        .expect("sync downloads is non-empty")
        .expect_err("the later inbound twin replaces the canonical sync body");
    assert!(matches!(
        sync_error,
        BlockDownloadVerifyError::Invalid {
            error: zebra_consensus::RouterError::Checkpoint { source },
            ..
        } if matches!(source.as_ref(), zebra_consensus::VerifyCheckpointError::NewerRequest { .. })
    ));

    // The inbound downloader reports the peer that served the twin once its verifier future
    // has finished, so the state has already rejected the twin, and the verifier has been reset.
    let report = timeout(MAX_STATE_COMMIT_DELAY, async {
        loop {
            inbound.ready().await?;
            if let Ok(report) = misbehavior_rx.try_recv() {
                return Ok::<_, crate::BoxError>(report);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the twin's peer is reported")?;
    assert_eq!((report.addr, report.score), (attacker_h, 100));

    // The honest block and its dropped child must not be reported as known any more.
    for hash in [honest.hash(), child.hash()] {
        let mut known = None;
        for _ in 0..20 {
            known = Some(
                state_service
                    .ready()
                    .await?
                    .call(zs::Request::KnownBlock(hash))
                    .await?,
            );
            if known == Some(zs::Response::KnownBlock(None)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(known, Some(zs::Response::KnownBlock(None)));
    }

    // The syncer downloads the honest block and its child again, and they commit.
    for block in [&honest, &child] {
        sync_downloads
            .download_and_verify(block.hash())
            .await
            .expect("sync download is queued");
        peer_set_handle
            .expect_request(zn::Request::BlockDownload {
                hash: block.hash(),
                advertiser: None,
            })
            .await
            .respond(zn::Response::Blocks(vec![Available((block.clone(), None))]));
    }
    // The honest block commits. The verifier queued the child's dropped copy again, so the
    // re-downloaded child either commits, or arrives after the queued copy completed the final
    // checkpoint range. If the child's first copy reached the state after the reset, the state
    // failed it instead of queueing it, so it can't hold up the re-downloaded copy.
    let mut committed = Vec::new();
    for _ in 0..2 {
        let result = timeout(MAX_STATE_COMMIT_DELAY, sync_downloads.next())
            .await
            .expect("re-downloaded block resolves")
            .expect("sync downloads is non-empty");
        match result {
            Ok((height, _)) => committed.push(height),
            Err(BlockDownloadVerifyError::Invalid {
                error: zebra_consensus::RouterError::Checkpoint { source },
                hash,
                ..
            }) if hash == child.hash()
                && matches!(
                    source.as_ref(),
                    zebra_consensus::VerifyCheckpointError::Finished
                ) => {}
            Err(error) => panic!("re-downloaded block commits: {error:?}"),
        }
    }
    assert!(committed.contains(&honest.coinbase_height().unwrap()));
    timeout(MAX_STATE_COMMIT_DELAY, async {
        while latest_chain_tip.best_tip_height() != child.coinbase_height() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the tip reaches the child");

    peer_set_handle.expect_no_requests().await;
    consensus_handles.state_checkpoint_verify_handle.abort();

    Ok(())
}

/// Build a downloader whose state tip is at `tip_height`, returning the mock network, verifier, and
/// state services so a test can drive and assert on them.
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

    let (past_lookahead_limit_sender, _past_lookahead_limit_receiver) = watch::channel(false);

    // `max_checkpoint_height` is far above the test blocks, so the final-checkpoint verify timeout
    // workaround does not apply.
    let downloads = Downloads::new(
        network.clone(),
        verifier.clone(),
        state.clone(),
        latest_chain_tip,
        past_lookahead_limit_sender,
        MIN_CHECKPOINT_CONCURRENCY_LIMIT,
        Height(1_000_000),
    );

    (downloads, network, verifier, state)
}

/// Negotiated sync downloads select authenticated validation without losing serving attribution.
#[tokio::test]
async fn early_relay_download_preserves_validation_and_source() {
    let _init_guard = zebra_test::init();
    for (early, admitted) in [(false, false), (true, false), (true, true)] {
        let (mut downloads, mut network, mut verifier, _) = mock_downloads(Height(0));
        let block = block_1();
        let hash = block.hash();
        let peer: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
        downloads.download_and_verify(hash).await.unwrap();
        let blocks = vec![Available((block.clone(), Some(peer)))];
        network
            .expect_request(zn::Request::BlockDownload {
                hash,
                advertiser: None,
            })
            .await
            .respond(if early {
                zn::Response::EarlyBlocks(blocks)
            } else {
                zn::Response::Blocks(blocks)
            });
        let request = if early {
            zebra_consensus::Request::CommitEarlyRelay(block)
        } else {
            zebra_consensus::Request::Commit(block)
        };
        let error = zebra_consensus::VerifyBlockError::Transaction(
            zebra_consensus::error::TransactionError::Groth16("invalid proof".into()),
        );
        let error = if admitted {
            zebra_consensus::VerifyBlockError::EarlyRelay(Box::new(error))
        } else {
            error
        };
        verifier
            .expect_request(request)
            .await
            .respond_error(zebra_consensus::RouterError::from(error).into());
        let error = downloads.next().await.unwrap().unwrap_err();
        assert!(
            matches!(
                error,
                BlockDownloadVerifyError::Invalid {
                    error,
                    advertiser_addr: Some(source),
                    ..
                } if source == peer && error.misbehavior_score() == if admitted { 0 } else { 100 }
            ),
            "only authenticated early-relay transaction failures are non-punitive"
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

/// A failed primary frees active-attempt capacity, not admission for another logical download.
#[tokio::test(start_paused = true)]
async fn logical_download_limit_holds_until_completion_or_cancellation() {
    let _init_guard = zebra_test::init();
    let mut config = crate::config::ZebradConfig::default();
    config.sync.download_concurrency_limit = 1;
    let mut peers: MockNetwork = MockService::build().for_unit_tests();
    let verifier: MockVerifier = MockService::build().for_unit_tests();
    let state: MockService<zs::Request, zs::Response, _> = MockService::build().for_unit_tests();
    let read_state: MockState = MockService::build().for_unit_tests();
    let (tip, _tip_sender) = MockChainTip::new();
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (mut chain_sync, _status) = ChainSync::new(
        &config,
        Height(0),
        peers.clone(),
        verifier,
        state,
        read_state,
        tip,
        misbehavior_tx,
    );
    let network = &mut chain_sync.downloads.network;

    // Warm the actual production stack with a positive delay, then rotate its histogram.
    for _ in 0..20 {
        let mut response = Box::pin(network.ready().await.unwrap().call(zn::Request::Peers));
        assert!(response.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_millis(100)).await;
        peers
            .expect_request(zn::Request::Peers)
            .await
            .respond(zn::Response::Peers(Vec::new()));
        tokio::time::timeout(MAX_SERVICE_REQUEST_DELAY, response)
            .await
            .unwrap()
            .unwrap();
    }
    tokio::time::advance(2 * SYNC_RESTART_DELAY).await;

    for succeeds in [Some(true), Some(false), None] {
        let hash = block::Hash::from([0xAB; 32]);
        let request = zn::Request::BlockDownload {
            hash,
            advertiser: None,
        };
        let mut pending = Box::pin(network.ready().await.unwrap().call(request.clone()));
        for _ in 0..=BLOCK_DOWNLOAD_RETRY_LIMIT {
            tokio::select! {
                result = &mut pending => panic!("the primary must not finish the hedge: {result:?}"),
                response = peers.expect_request(request.clone()) => {
                    response.respond(Err::<zn::Response, _>(zn::BoxError::from("exhausted")));
                }
            }
        }
        assert!(pending.as_mut().now_or_never().is_none());
        assert!(
            network.ready().now_or_never().is_none(),
            "a pending delayed arm must retain the logical download permit",
        );

        if let Some(succeeds) = succeeds {
            tokio::time::advance(Duration::from_millis(100)).await;
            let retries = if succeeds {
                0
            } else {
                BLOCK_DOWNLOAD_RETRY_LIMIT
            };
            for _ in 0..=retries {
                tokio::select! {
                    result = &mut pending => panic!("the delayed arm must be served: {result:?}"),
                    response = peers.expect_request(request.clone()) => {
                        response.respond(if succeeds {
                            Ok(zn::Response::Peers(Vec::new()))
                        } else {
                            Err(zn::BoxError::from("exhausted"))
                        });
                    }
                }
            }
            assert_eq!(
                tokio::time::timeout(MAX_SERVICE_REQUEST_DELAY, pending)
                    .await
                    .unwrap()
                    .is_ok(),
                succeeds,
            );
        } else {
            drop(pending);
        }
        assert!(
            network.ready().now_or_never().is_some(),
            "success, final error, and cancellation must release the logical download permit",
        );
    }
}

/// Regression test for GHSA-g95h-hw6g-pvgv: a body whose claimed height contradicts the parent we
/// already hold is attributed to the peer that supplied it, and never reaches consensus.
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

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // We hold the real parent, block 1, so the body's real height is 2, not the 1 it claims.
    state
        .expect_request(zs::ReadRequest::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::ReadResponse::BlockHeader(Some(zs::BlockHeader {
            header: block_1.header.clone(),
            hash: block_1.hash(),
            height: Height(1),
            next_block_hash: Some(hash),
        })));

    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                height: Height(1),
                hash: error_hash,
                advertiser_addr: Some(error_advertiser),
            } if error_hash == hash && error_advertiser == advertiser
        ),
        "a contradicted behind-tip height must report the height, hash, and supplying peer, \
         but was: {error:?}"
    );

    // The rewritten body is dropped before consensus validation, which is why the peer must be
    // scored on this path instead.
    verifier.expect_no_requests().await;
}

/// A peer that serves a genuinely old block is not attributed, so it cannot be scored.
///
/// The syncer tolerates an out-of-order first hash in `FindBlocks` responses, so a requested hash
/// can resolve to an old block that an honest peer still holds. Its height agrees with its parent's,
/// so there is no proof of misbehaviour and the drop must stay anonymous.
#[tokio::test(start_paused = true)]
async fn genuinely_old_block_is_dropped_without_attribution() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
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
        .expect_request(zs::ReadRequest::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::ReadResponse::BlockHeader(Some(zs::BlockHeader {
            header: genesis.header.clone(),
            hash: genesis.hash(),
            height: Height(0),
            next_block_hash: Some(hash),
        })));

    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                advertiser_addr: None,
                ..
            }
        ),
        "an authentic old block must not be attributed to its peer, but was: {error:?}"
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

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    state
        .expect_request(zs::ReadRequest::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::ReadResponse::BlockHeader(None));

    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                advertiser_addr: None,
                ..
            }
        ),
        "a block whose parent we do not hold must not be attributed, but was: {error:?}"
    );

    verifier.expect_no_requests().await;
}

/// The parent lookup is bounded: when the state does not answer, the block is still dropped and
/// the peer is still not attributed.
///
/// Paused time lets the runtime advance past `PARENT_LOOKUP_TIMEOUT` as soon as the download task
/// is the only thing waiting, so the test does not sleep for real.
#[tokio::test(start_paused = true)]
async fn behind_tip_parent_lookup_timeout_is_not_attributed() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, _state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The state service is deliberately never answered, so the lookup can only end by timing out.
    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                advertiser_addr: None,
                ..
            }
        ),
        "a timed-out parent lookup must not attribute the drop, but was: {error:?}"
    );

    verifier.expect_no_requests().await;
}

/// A block at the oldest height the syncer can legitimately be served is verified, not dropped.
///
/// This is the guard against dropping and attributing honest near-boundary blocks:
/// `min_accepted_height` is the boundary, and the behind-tip comparison is strict, so a block
/// exactly on it is a normal download and the state is never consulted.
#[tokio::test(start_paused = true)]
async fn block_at_reorg_boundary_is_verified_not_dropped() {
    let _init_guard = zebra_test::init();

    // `min_accepted_height` is `Height(1)`, so the height-1 block sits exactly on the boundary.
    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(MAX_BLOCK_REORG_HEIGHT + 1));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

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
        (Height(1), hash),
        "a block at min_accepted_height must be verified, not dropped as behind the tip"
    );

    state.expect_no_requests().await;
}

/// Network readiness and an unanswered response share one absolute download deadline.
#[tokio::test(start_paused = true)]
async fn download_deadline_includes_readiness_and_response() {
    use tokio::time::{sleep, timeout_at, Instant};
    use tower::{limit::ConcurrencyLimit, ServiceExt};

    let _init_guard = zebra_test::init();
    let budget = super::BLOCK_DOWNLOAD_ROUND_TIMEOUT;
    for release_capacity in [false, true] {
        let mut network: MockNetwork = MockService::build().for_unit_tests();
        let limited = ConcurrencyLimit::new(network.clone(), 1);
        let mut reservation = limited.clone();
        reservation.ready().await.unwrap();
        let verifier: MockVerifier = MockService::build().for_unit_tests();
        let state: MockState = MockService::build().for_unit_tests();
        let (tip, _sender) = MockChainTip::new();
        let mut downloads = Downloads::new(
            limited,
            verifier,
            state,
            tip,
            watch::channel(false).0,
            MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            Height(1_000_000),
        );
        let hash = block_1().hash();
        let started = Instant::now();
        let queued = {
            let enqueue = downloads.download_and_verify(hash);
            tokio::pin!(enqueue);
            assert!(futures::poll!(&mut enqueue).is_pending());
            if release_capacity {
                sleep(budget / 2).await;
                drop(reservation);
            }
            timeout_at(started + budget + Duration::from_secs(1), enqueue)
                .await
                .expect("network readiness must be bounded")
        };
        let result = if release_capacity {
            queued.expect("capacity becomes available before the deadline");
            let _response = network
                .expect_request(zn::Request::BlockDownload {
                    hash,
                    advertiser: None,
                })
                .await;
            timeout_at(started + budget + Duration::from_secs(1), downloads.next())
                .await
                .expect("the response must use the remaining deadline")
                .expect("the queued download has a result")
                .map(|_| ())
        } else {
            queued
        };
        // Readiness timeouts are returned by `download_and_verify` itself, but response timeouts
        // are re-requestable download failures.
        if release_capacity {
            assert!(matches!(
                result,
                Err(BlockDownloadVerifyError::DownloadTimedOut { hash: timed_out }) if timed_out == hash
            ));
        } else {
            assert!(matches!(result, Err(BlockDownloadVerifyError::Timeout)));
        }
        assert_eq!(Instant::now() - started, budget);
    }
}

/// Checkpoint and orphan admission pressure retry the body without another peer request.
#[tokio::test(start_paused = true)]
async fn checkpoint_and_orphan_pressure_retry_without_redownload() {
    let _init_guard = zebra_test::init();
    let (mut downloads, mut network, mut verifier, mut state) = mock_downloads(Height(0));
    let block = block_1();
    let hash = block.hash();
    downloads.download_and_verify(hash).await.unwrap();
    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((block.clone(), None))]));

    for error in [
        zebra_consensus::RouterError::from(zebra_consensus::VerifyCheckpointError::QueuedLimit),
        zebra_consensus::RouterError::from(zebra_consensus::VerifyBlockError::Commit(
            zs::CommitBlockError::OrphanQueueFull,
        )),
    ] {
        assert!(error.is_resource_pressure());
        assert_eq!(error.misbehavior_score(), 0);
        verifier
            .expect_request(zebra_consensus::Request::Commit(block.clone()))
            .await
            .respond_error(error.into());
    }
    verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(hash);
    assert_eq!(downloads.next().await.unwrap().unwrap(), (Height(1), hash));
    assert!(downloads.is_empty());
    network.expect_no_requests().await;
    state.expect_no_requests().await;
}

/// Retrying admission must not grant another full verification deadline.
#[tokio::test(start_paused = true)]
async fn resource_pressure_retry_keeps_original_verify_deadline() {
    let _init_guard = zebra_test::init();
    let (mut downloads, mut network, mut verifier, _) = mock_downloads(Height(0));
    let block = block_1();
    let hash = block.hash();
    downloads.download_and_verify(hash).await.unwrap();
    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((block.clone(), None))]));
    let first_request = verifier
        .expect_request(zebra_consensus::Request::Commit(block.clone()))
        .await;
    let started = tokio::time::Instant::now();
    first_request.respond_error(
        zebra_consensus::RouterError::from(zebra_consensus::VerifyCheckpointError::QueuedLimit)
            .into(),
    );
    let pending = verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await;
    let error = downloads.next().await.unwrap().unwrap_err();
    assert!(matches!(
        error,
        BlockDownloadVerifyError::ValidationRequestError { error, .. }
            if error.is::<tower::timeout::error::Elapsed>()
    ));
    assert_eq!(started.elapsed(), super::BLOCK_VERIFY_TIMEOUT);
    pending.respond(hash);
    network.expect_no_requests().await;
}

/// Cancellation removes the existing download task, including its pressure backoff and body.
#[tokio::test(start_paused = true)]
async fn cancelling_pressure_retry_releases_downloaded_body() {
    let _init_guard = zebra_test::init();
    let (mut downloads, mut network, mut verifier, _) = mock_downloads(Height(0));
    let block = block_1();
    let body = Arc::downgrade(&block);
    let hash = block.hash();
    downloads.download_and_verify(hash).await.unwrap();
    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((block.clone(), None))]));
    verifier
        .expect_request(zebra_consensus::Request::Commit(block.clone()))
        .await
        .respond_error(
            zebra_consensus::RouterError::from(zebra_consensus::VerifyCheckpointError::QueuedLimit)
                .into(),
        );
    drop(block);
    downloads.cancel_all();
    tokio::time::timeout(MAX_SERVICE_REQUEST_DELAY, async {
        while body.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelling pressure retries must release their retained downloaded body");
    assert!(downloads.is_empty());
    verifier.expect_no_requests().await;
    network.expect_no_requests().await;
}
