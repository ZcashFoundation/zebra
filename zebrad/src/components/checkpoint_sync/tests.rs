//! Supervisor tests: the claim protocol, the hand-back, and the
//! fast-forward checkpoint.

use std::{sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::sync::{mpsc, watch};

use zebra_chain::{block, chain_tip::mock::MockChainTip, parameters::Network};

use super::{
    engine::tests::{
        mock_state, mock_state_with_stall, mock_state_with_tip, Behaviour, MockFailure, MockPeer,
        TestChain,
    },
    *,
};

#[tokio::test]
async fn supervisor_claims_runs_and_hands_back() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let peers: Vec<MockPeer> = (1..=2)
        .map(|id| MockPeer::new(id, chain.clone(), Behaviour::Honest))
        .collect();
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(peers));

    let (latest_chain_tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(block::Height(0));
    tip_sender.send_best_tip_hash(chain.hash(0));

    // The state moves the tip as it commits.
    let (commits_tx, mut committed_rx) = mpsc::unbounded_channel();
    let state = mock_state_with_tip(commits_tx, |_, _| None, Some(tip_sender));

    let (engine_side, mut legacy, mut checkpoint_tip) = ownership_channels();
    let (_gate_tx, gate_rx) = watch::channel(true);

    let mut supervisor = Supervisor::new(
        EngineConfig::default(),
        Network::Mainnet,
        peers_rx,
        state,
        latest_chain_tip,
        engine_side,
        // A hints gate makes the supervisor claim despite the short span.
        Some(gate_rx),
    );
    supervisor.anchors = Anchors::new(&chain.list);
    let task = tokio::spawn(supervisor.run());

    // Claiming: the engine waits for the legacy syncer's acknowledgement.
    let claim = *legacy
        .owner
        .wait_for(|owner| !owner.is_legacy())
        .await
        .unwrap();
    assert_eq!(
        claim,
        SyncOwner::Engine {
            target: block::Height(10),
            generation: 1,
        }
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        committed_rx.try_recv().is_err(),
        "nothing commits before the ack"
    );
    legacy.ack.send_replace(1);

    // Draining hands back with the durable checkpoint.
    tokio::time::timeout(
        Duration::from_secs(30),
        legacy.owner.wait_for(SyncOwner::is_legacy),
    )
    .await
    .expect("the engine hands back")
    .unwrap();
    assert_eq!(
        *checkpoint_tip.borrow_and_update(),
        Some(CommittedTip {
            height: block::Height(10),
            hash: chain.hash(10),
            hand_back: Some(1),
        })
    );

    let mut lengths = Vec::new();
    while let Ok(length) = legacy.unit_lengths.try_recv() {
        lengths.push(length);
    }
    assert_eq!(lengths, vec![5, 5]);

    assert!(!task.is_finished(), "the supervisor keeps running");
    task.abort();
}

#[tokio::test]
async fn supervisor_stays_idle_without_peers_or_span() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let peers: Vec<MockPeer> = (1..=2)
        .map(|id| MockPeer::new(id, chain.clone(), Behaviour::Honest))
        .collect();
    let (peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(Vec::new()));

    let (latest_chain_tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(block::Height(0));
    tip_sender.send_best_tip_hash(chain.hash(0));

    let (commits_tx, _commits_rx) = mpsc::unbounded_channel();
    let (engine_side, legacy, _checkpoint_tip) = ownership_channels();
    let mut supervisor = Supervisor::new(
        EngineConfig::default(),
        Network::Mainnet,
        peers_rx,
        mock_state(commits_tx, |_, _| false),
        latest_chain_tip,
        engine_side,
        None,
    );
    supervisor.anchors = Anchors::new(&chain.list);

    // No peers: no claim.
    assert_eq!(supervisor.claim_target(block::Height(0)), None);

    // On Mainnet, two peers, but one below the first interval: no claim.
    supervisor.anchors = Anchors::new(&Network::Mainnet.checkpoint_list());
    let first_checkpoint = supervisor.anchors.at_or_above(block::Height(1)).unwrap().0;
    peers_tx.send_replace(Arc::from(vec![
        MockPeer::new(1, chain.clone(), Behaviour::Honest).with_tip(block::Height(1_000_000)),
        MockPeer::new(2, chain.clone(), Behaviour::Honest)
            .with_tip(first_checkpoint.previous().unwrap()),
    ]));
    assert_eq!(supervisor.claim_target(block::Height(0)), None);
    supervisor.anchors = Anchors::new(&chain.list);

    // Two peers, but a span of 2 intervals, below `MIN_ENGINE_SPAN_INTERVALS`.
    peers_tx.send_replace(Arc::from(peers));
    assert_eq!(supervisor.claim_target(block::Height(0)), None);

    // On Mainnet, the span from genesis to the peers' tip is large.
    let peers: Vec<MockPeer> = (1..=2)
        .map(|id| {
            MockPeer::new(id, chain.clone(), Behaviour::Honest).with_tip(block::Height(1_000_000))
        })
        .collect();
    peers_tx.send_replace(Arc::from(peers));
    supervisor.anchors = Anchors::new(&Network::Mainnet.checkpoint_list());
    let target = supervisor
        .claim_target(block::Height(0))
        .expect("a claim on Mainnet");
    assert!(target.0 <= 1_000_000 - anchors::PEER_TIP_MARGIN);
    assert!(supervisor.anchors.contains(target));

    assert!(legacy.owner.borrow().is_legacy());
}

#[tokio::test]
async fn supervisor_hands_back_at_the_durable_tip_after_a_mid_interval_failure() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let peers: Vec<MockPeer> = (1..=2)
        .map(|id| MockPeer::new(id, chain.clone(), Behaviour::Honest))
        .collect();
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(peers));

    let (latest_chain_tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(block::Height(0));
    tip_sender.send_best_tip_hash(chain.hash(0));

    // Height 3, inside the interval ending at checkpoint 5, always fails:
    // the state's tip stops at 2.
    let (commits_tx, _commits_rx) = mpsc::unbounded_channel();
    let state = mock_state_with_tip(
        commits_tx,
        |height, _| (height.0 == 3).then_some(MockFailure::Failed),
        Some(tip_sender),
    );

    let (engine_side, legacy, checkpoint_tip) = ownership_channels();
    let (_gate_tx, gate_rx) = watch::channel(true);
    let mut supervisor = Supervisor::new(
        EngineConfig::default(),
        Network::Mainnet,
        peers_rx,
        state,
        latest_chain_tip,
        engine_side,
        Some(gate_rx),
    );
    supervisor.anchors = Anchors::new(&chain.list);
    legacy.ack.send_replace(1);

    let result = tokio::time::timeout(Duration::from_secs(30), supervisor.run())
        .await
        .expect("the supervisor stops on the deterministic failure");
    assert!(result.is_err(), "a deterministic failure is a defect");

    assert!(legacy.owner.borrow().is_legacy());
    assert_eq!(
        *checkpoint_tip.borrow(),
        Some(CommittedTip {
            height: block::Height(2),
            hash: chain.hash(2),
            hand_back: Some(1),
        }),
        "the verifier resumes at the state tip, not at the checkpoint below it"
    );
}

/// Peers the last run blamed for its lack of progress do not count toward
/// the next claim, even over a new connection.
#[tokio::test]
async fn supervisor_does_not_reclaim_with_only_blamed_peers() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let peers: Vec<MockPeer> = (1..=3)
        .map(|id| {
            MockPeer::new(id, chain.clone(), Behaviour::Honest).with_tip(block::Height(1_000_000))
        })
        .collect();
    let ips: Vec<_> = peers.iter().map(MockPeer::peer_ip).collect();
    let (peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(peers.clone()));

    let (latest_chain_tip, _tip_sender) = MockChainTip::new();
    let (commits_tx, _commits_rx) = mpsc::unbounded_channel();
    let (engine_side, _legacy, _checkpoint_tip) = ownership_channels();
    let mut supervisor = Supervisor::new(
        EngineConfig::default(),
        Network::Mainnet,
        peers_rx,
        mock_state(commits_tx, |_, _| false),
        latest_chain_tip,
        engine_side,
        None,
    );
    assert!(supervisor.claim_target(block::Height(0)).is_some());

    // Two of the three peers were blamed: one unblamed peer is too few.
    supervisor.blamed = [ips[0], ips[1]].into_iter().collect();
    assert_eq!(supervisor.claim_target(block::Height(0)), None);

    // A blamed peer that reconnects still does not count.
    let reconnected = MockPeer::new(4, chain.clone(), Behaviour::Honest)
        .with_tip(block::Height(1_000_000))
        .with_ip(ips[0]);
    peers_tx.send_replace(peers.into_iter().chain([reconnected]).collect());
    assert_eq!(supervisor.claim_target(block::Height(0)), None);

    supervisor.blamed = [ips[0]].into_iter().collect();
    assert!(supervisor.claim_target(block::Height(0)).is_some());
}

/// A commit that an engine run timed out on can land after the hand-back:
/// the supervisor republishes the state tip, so the checkpoint verifier does
/// not wait for heights the state already has.
#[tokio::test]
async fn supervisor_republishes_a_tip_that_a_timed_out_commit_moves_after_hand_back() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    let chain = TestChain::new();
    let peers: Vec<MockPeer> = (1..=2)
        .map(|id| MockPeer::new(id, chain.clone(), Behaviour::Honest))
        .collect();
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(peers));

    let (latest_chain_tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(block::Height(0));
    tip_sender.send_best_tip_hash(chain.hash(0));

    // The state's write task stalls on height 6 until the test releases it,
    // past both of the engine's commit timeouts, then commits 6 to 10, which
    // it had already queued.
    let (release_tx, release_rx) = watch::channel(false);
    let (commits_tx, _commits_rx) = mpsc::unbounded_channel();
    let state = mock_state_with_stall(
        commits_tx,
        |_, _| None,
        Some(tip_sender),
        move |height, attempt| {
            let mut release = release_rx.clone();
            (height.0 == 6 && attempt == 0).then(|| {
                async move {
                    let _ = release.wait_for(|released| *released).await;
                }
                .boxed()
            })
        },
    );

    let (engine_side, mut legacy, mut checkpoint_tip) = ownership_channels();
    let (_gate_tx, gate_rx) = watch::channel(true);
    let mut supervisor = Supervisor::new(
        EngineConfig::default(),
        Network::Mainnet,
        peers_rx,
        state,
        latest_chain_tip,
        engine_side,
        Some(gate_rx),
    );
    supervisor.anchors = Anchors::new(&chain.list);
    legacy.ack.send_replace(1);
    let task = tokio::spawn(supervisor.run());

    // The run gives up on the stalled commits, and hands back at 5.
    legacy
        .owner
        .wait_for(|owner| !owner.is_legacy())
        .await
        .unwrap();
    legacy.owner.wait_for(SyncOwner::is_legacy).await.unwrap();
    assert_eq!(
        *checkpoint_tip.borrow_and_update(),
        Some(CommittedTip {
            height: block::Height(5),
            hash: chain.hash(5),
            hand_back: Some(1),
        })
    );

    // The stalled commits land after the hand-back, and the verifier's tip
    // follows them.
    release_tx.send_replace(true);
    tokio::time::timeout(
        Duration::from_secs(600),
        checkpoint_tip.wait_for(|tip| tip.is_some_and(|tip| tip.height == block::Height(10))),
    )
    .await
    .expect("the supervisor republishes the late commits")
    .unwrap();
    assert_eq!(
        *checkpoint_tip.borrow(),
        Some(CommittedTip {
            height: block::Height(10),
            hash: chain.hash(10),
            hand_back: Some(1),
        })
    );

    assert!(!task.is_finished(), "the supervisor keeps running");
    task.abort();
}
