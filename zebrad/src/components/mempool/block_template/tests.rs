//! Regression checks for template scheduling, publication, and retained coinbase proofs.

use zcash_keys::address::Address;
use zebra_chain::{
    parameters::{testnet::ConfiguredActivationHeights, NetworkUpgrade},
    serialization::Duration32,
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, ParameterDifficulty, U256},
};
use zebra_rpc::config::mining::{default_miner_address, MinerAddressType};
use zebra_state::GetBlockTemplateChainInfo;
use zebra_test::mock_service::{MockService, PanicAssertion, ResponseSender};

use super::*;

fn parameters() -> (Network, MinerParams, Height) {
    let network = Network::Mainnet;
    let miner_params = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Transparent),
        )
        .expect("the hard-coded transparent address is valid"),
    );
    let height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("NU5 is active on mainnet");
    (network, miner_params, height)
}

/// A reorg must not detach a proof that can still be reused at its original height.
#[tokio::test]
async fn in_flight_coinbase_is_retained_across_height_changes() {
    let _init_guard = zebra_test::init();
    let (network, miner_params, height) = parameters();
    let other_height = height.next().expect("test height is below the maximum");
    let coinbase =
        TransactionTemplate::new_coinbase(&network, height, &miner_params, Amount::zero())
            .expect("test parameters produce a valid coinbase");
    let expected_coinbase = coinbase.clone();
    let cache = CoinbaseCache::default();
    cache.select(height, None);
    let (release_proof, proof_released) = tokio::sync::oneshot::channel();
    let mut next_coinbase = Some((
        height,
        tokio::task::spawn_blocking(move || {
            proof_released
                .blocking_recv()
                .expect("the test releases the proof before awaiting it");
            coinbase
        }),
    ));

    timeout(Duration::from_secs(10), async {
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, other_height);
        release_proof.send(()).expect("the proof is still waiting");
        store_precomputed_coinbase(&mut next_coinbase, height, &cache).await;

        assert_eq!(
            cache.get(height, Amount::zero(), None),
            Some(expected_coinbase)
        );
        assert!(cache.get(other_height, Amount::zero(), None).is_none());
    })
    .await
    .expect("the retained proof completes once released");
}

/// Completed stale work must neither enter the wrong cache entry nor prevent the next proof.
#[tokio::test]
async fn completed_coinbase_is_replaced_without_caching_the_wrong_height() {
    let _init_guard = zebra_test::init();
    let (network, miner_params, height) = parameters();
    let other_height = height.next().expect("test height is below the maximum");
    let cache = CoinbaseCache::default();
    cache.select(other_height, None);
    let mut next_coinbase = None;
    start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, height);

    timeout(Duration::from_secs(10), async {
        while !next_coinbase
            .as_ref()
            .expect("a proof was started")
            .1
            .is_finished()
        {
            tokio::task::yield_now().await;
        }
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        assert!(cache.get(height, Amount::zero(), None).is_none());
        assert!(cache.get(other_height, Amount::zero(), None).is_none());

        start_precomputing_coinbase(&mut next_coinbase, &network, &miner_params, other_height);
        store_precomputed_coinbase(&mut next_coinbase, other_height, &cache).await;
        assert_eq!(
            cache.get(other_height, Amount::zero(), None),
            Some(
                TransactionTemplate::new_coinbase(
                    &network,
                    other_height,
                    &miner_params,
                    Amount::zero(),
                )
                .expect("test parameters produce a valid coinbase")
            ),
        );
    })
    .await
    .expect("the replacement proof completes");
}

type ProposalVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;
type Proposal = ResponseSender<zebra_consensus::Request, block::Hash, BoxError>;

/// Real template construction, with controllable consensus completion and chain information.
struct Scheduler {
    templates: BlockTemplates,
    published: watch::Receiver<Option<Arc<BlockTemplateResponse>>>,
    requests: mpsc::Sender<BlockTemplateRequest>,
    verifier: ProposalVerifier,
    chain_info: watch::Sender<GetBlockTemplateChainInfo>,
    storage: Storage,
    checked: Vec<Arc<block::Block>>,
}

impl Scheduler {
    fn new(network: Network) -> Self {
        let miner_params = MinerParams::from(
            Address::decode(
                &network,
                default_miner_address(network.kind(), &MinerAddressType::Transparent),
            )
            .unwrap(),
        );
        let (chain_info, state_info) = watch::channel(GetBlockTemplateChainInfo {
            tip_height: NetworkUpgrade::Nu5.activation_height(&network).unwrap(),
            tip_hash: block::Hash([0xab; 32]),
            chain_history_root: Some([0; 32].into()),
            expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
            cur_time: DateTime32::from(1_654_008_617),
            min_time: DateTime32::from(1_654_003_320),
            max_time: DateTime32::from(1_654_008_719),
            chain_value_pools: Default::default(),
        });
        let state = tower::service_fn(move |request| {
            let info = state_info.borrow().clone();
            async move {
                Ok::<_, BoxError>(match request {
                    ReadRequest::ChainInfo => ReadResponse::ChainInfo(info),
                    ReadRequest::Tip => ReadResponse::Tip(Some((info.tip_height, info.tip_hash))),
                    _ => panic!("unexpected template state request"),
                })
            }
        });
        let verifier = MockService::build().for_unit_tests();
        let (templates, published, requests) = BlockTemplates::new(
            network,
            Some(miner_params),
            Buffer::new(BoxService::new(state), 1),
            Buffer::new(BoxService::new(verifier.clone()), 1),
        );
        Self {
            templates,
            published,
            requests,
            verifier,
            chain_info,
            storage: Storage::new(&super::super::Config {
                tx_cost_limit: u64::MAX,
                ..Default::default()
            }),
            checked: Vec::new(),
        }
    }

    fn poll(&mut self) {
        let tip = self.chain_info.borrow().tip_hash;
        assert!(matches!(
            self.templates.poll(
                &mut Context::from_waker(futures::task::noop_waker_ref()),
                Some((&self.storage, tip)),
                Some(tip),
            ),
            Poll::Ready(Ok(()))
        ));
    }

    /// Drain started builds, recording real proposal requests, without advancing the paused clock.
    async fn idle(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            self.poll();
            if let Some(Some(proposal)) = self.verifier.try_next_request().now_or_never() {
                let zebra_consensus::Request::CheckProposal(block) = proposal.request() else {
                    panic!("templates only submit proposals");
                };
                self.checked.push(block.clone());
                proposal.respond(block::Hash([0; 32]));
            }
            if self.templates.build.is_none() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "template build stalled"
            );
            tokio::task::yield_now().await;
        }
    }

    async fn proposal(&mut self) -> Proposal {
        let tip = self.chain_info.borrow().tip_hash;
        drive(
            &mut self.templates,
            &self.storage,
            tip,
            self.verifier.expect_request_that(|request| {
                matches!(request, zebra_consensus::Request::CheckProposal(_))
            }),
        )
        .await
    }

    fn override_request(
        &self,
    ) -> tokio::sync::oneshot::Receiver<Result<BlockTemplateResponse, BoxError>> {
        let (response, result) = tokio::sync::oneshot::channel();
        self.requests
            .try_send(BlockTemplateRequest {
                miner_params: self.templates.miner_params.clone().unwrap(),
                response,
            })
            .unwrap();
        result
    }
}

async fn drive<F: Future>(
    templates: &mut BlockTemplates,
    storage: &Storage,
    tip: block::Hash,
    future: F,
) -> F::Output {
    tokio::pin!(future);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(matches!(
            templates.poll(
                &mut Context::from_waker(futures::task::noop_waker_ref()),
                Some((storage, tip)),
                Some(tip),
            ),
            Poll::Ready(Ok(()))
        ));
        if let Some(result) = future.as_mut().now_or_never() {
            return result;
        }
        // A wall-clock bound also covers spawn_blocking while Tokio time remains paused.
        assert!(
            std::time::Instant::now() < deadline,
            "template build stalled"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn same_tip_changes_are_coalesced_without_losing_latest_membership() {
    let _init_guard = zebra_test::init();
    let mut scheduler = Scheduler::new(Network::Mainnet);
    let first = scheduler.proposal().await;
    let mut tx = Network::Mainnet
        .unmined_transactions_in_blocks(982_681..=982_681)
        .find(|tx| {
            !tx.transaction.transaction.is_coinbase()
                && !tx.transaction.transaction.outputs().is_empty()
        })
        .expect("the historical block fixture contains a transparent spend");
    tx.miner_fee = tx.transaction.conventional_fee;
    tx.fee_weight_ratio = 1.0;
    tx.unpaid_actions = 0;
    let txid = tx.transaction.id;
    // A change arriving during the first coinbase build still gets the immediate initial fill.
    scheduler.storage.insert(tx.clone(), vec![], None).unwrap();
    scheduler.templates.mark_changed();
    first.respond(block::Hash([0; 32]));
    let fill = scheduler.proposal().await;
    assert!(scheduler
        .published
        .borrow()
        .as_ref()
        .unwrap()
        .transactions()
        .is_empty());
    // A change during the fill waits for the debounce, without losing the newest membership.
    scheduler
        .storage
        .remove_exact(&[txid].into_iter().collect());
    scheduler.templates.mark_changed();
    fill.respond(block::Hash([0; 32]));
    scheduler.idle().await;
    assert!(
        scheduler.checked.is_empty(),
        "only the initial coinbase and fill were verified"
    );
    assert_eq!(
        proposal_block_from_template(
            scheduler.published.borrow().as_ref().unwrap(),
            None,
            &Network::Mainnet,
        )
        .unwrap()
        .transactions[1]
            .hash(),
        txid.mined_id(),
    );

    tokio::time::advance(CHANGE_DEBOUNCE - Duration::from_millis(1)).await;
    scheduler.templates.mark_changed();
    scheduler.idle().await;
    assert!(
        scheduler.checked.is_empty(),
        "a burst is coalesced before 500ms"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    scheduler.idle().await;
    assert_eq!(
        scheduler.checked.len(),
        1,
        "changes cannot slide the first deadline"
    );
    assert!(scheduler
        .published
        .borrow()
        .as_ref()
        .unwrap()
        .transactions()
        .is_empty());

    // A second burst gets its own deadline, and the build sees its final membership.
    scheduler.storage.insert(tx.clone(), vec![], None).unwrap();
    scheduler.templates.mark_changed();
    tokio::time::advance(CHANGE_DEBOUNCE / 2).await;
    scheduler
        .storage
        .remove_exact(&[txid].into_iter().collect());
    scheduler.templates.mark_changed();
    tokio::time::advance(CHANGE_DEBOUNCE / 2).await;
    scheduler.idle().await;
    assert_eq!(scheduler.checked.len(), 2);
    assert_eq!(scheduler.checked[1].transactions.len(), 1);

    // Without notifications the bounded backstop still revalidates and republishes.
    scheduler.storage.insert(tx, vec![], None).unwrap();
    tokio::time::advance(Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL)).await;
    scheduler.idle().await;
    assert_eq!(scheduler.checked.len(), 3);
    assert_eq!(scheduler.checked[2].transactions[1].hash(), txid.mined_id());

    // A changed parent bypasses the debounce, with coinbase work before one mempool fill.
    scheduler
        .chain_info
        .send_modify(|info| info.tip_hash = block::Hash([0xcd; 32]));
    scheduler.templates.mark_changed();
    scheduler.idle().await;
    assert_eq!(scheduler.checked.len(), 5);
    assert_eq!(scheduler.checked[3].transactions.len(), 1);
    assert_eq!(scheduler.checked[4].transactions[1].hash(), txid.mined_id());
}

#[tokio::test(start_paused = true)]
async fn default_recovery_wins_over_continuous_overrides_after_backoff() {
    let _init_guard = zebra_test::init();
    let mut scheduler = Scheduler::new(Network::Mainnet);
    scheduler
        .proposal()
        .await
        .respond_error("transient state failure".into());
    scheduler.poll();
    let mut current = scheduler.override_request();
    let mut proposal = scheduler.proposal().await;
    let mut queued = scheduler.override_request();
    let tip = scheduler.chain_info.borrow().tip_hash;

    for attempt in 0..4 {
        tokio::time::advance(RETRY_DELAY / 4).await;
        scheduler.templates.mark_changed();
        proposal.respond(block::Hash([0; 32]));
        drive(
            &mut scheduler.templates,
            &scheduler.storage,
            tip,
            &mut current,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            scheduler.published.borrow().is_none(),
            "override work must stay private"
        );
        proposal = scheduler.proposal().await;
        if attempt < 3 {
            current = queued;
            queued = scheduler.override_request();
        }
    }

    // The queue is still ready, but this proposal must be the now-eligible default recovery.
    proposal.respond(block::Hash([0; 32]));
    let mut published = scheduler.published.clone();
    let recovered = drive(&mut scheduler.templates, &scheduler.storage, tip, async {
        tokio::select! {
            result = published.changed() => {
                result.unwrap();
                true
            }
            _ = &mut queued => false,
        }
    })
    .await;
    assert!(
        recovered,
        "an eligible default retry must win over a queued override"
    );
    assert!(scheduler.published.borrow().is_some());
    scheduler.idle().await;
}

#[tokio::test(start_paused = true)]
async fn stale_overrides_rebuild_for_the_current_tip() {
    let _init_guard = zebra_test::init();
    for proposal_started in [false, true] {
        let mut scheduler = Scheduler::new(Network::Mainnet);
        let response = scheduler.override_request();
        scheduler.templates.miner_params = None;

        // Cover both a stale storage snapshot and a tip change during proposal verification.
        let proposal = if proposal_started {
            Some(scheduler.proposal().await)
        } else {
            scheduler.poll();
            None
        };
        scheduler.chain_info.send_modify(|info| {
            info.tip_height = info.tip_height.next().unwrap();
            info.tip_hash = block::Hash([0xcd; 32]);
        });
        if let Some(proposal) = proposal {
            proposal.respond(block::Hash([0; 32]));
        }
        scheduler.idle().await;
        tokio::time::advance(RETRY_DELAY).await;
        scheduler.idle().await;

        let template = response.await.unwrap().unwrap();
        let info = scheduler.chain_info.borrow();
        assert_eq!(template.previous_block_hash(), info.tip_hash);
        assert_eq!(template.height(), info.tip_height.next().unwrap().0);
        assert!(scheduler.published.borrow().is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn expired_testnet_completion_refetches_immediately_and_retains_proof() {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    let mut scheduler = Scheduler::new(network.clone());
    scheduler.chain_info.send_modify(|info| {
        info.min_time = info.max_time.saturating_sub(Duration32::from_seconds(100));
    });
    let height = (scheduler.chain_info.borrow().tip_height + 3).unwrap();
    let coinbase = TransactionTemplate::new_coinbase(
        &network,
        height,
        scheduler.templates.miner_params.as_ref().unwrap(),
        Amount::zero(),
    )
    .unwrap();
    let expected_coinbase = coinbase.clone();
    let (release, released) = tokio::sync::oneshot::channel();
    scheduler.templates.next_coinbase = Some((
        height,
        tokio::spawn(async move {
            released.await.unwrap();
            coinbase
        }),
    ));
    let stale = scheduler.proposal().await;
    scheduler.chain_info.send_modify(|info| {
        info.expected_difficulty = network.target_difficulty_limit().to_compact();
    });
    let completed_at = Instant::now();
    stale.respond(block::Hash([0; 32]));
    let replacement = scheduler.proposal().await;
    assert!(
        scheduler.published.borrow().is_none(),
        "expired work must never be published"
    );
    assert_eq!(
        Instant::now(),
        completed_at,
        "transition must not wait for the refresh timer"
    );
    let zebra_consensus::Request::CheckProposal(block) = replacement.request() else {
        unreachable!()
    };
    assert_eq!(
        block.header.difficulty_threshold,
        network.target_difficulty_limit().to_compact()
    );
    replacement.respond(block::Hash([0; 32]));
    scheduler.idle().await;
    assert!(scheduler
        .published
        .borrow()
        .as_ref()
        .unwrap()
        .is_valid_for_tip(
            scheduler.chain_info.borrow().tip_hash,
            &network,
            DateTime32::now(),
        ));

    let (retained_height, proof) = scheduler.templates.next_coinbase.take().unwrap();
    assert_eq!(retained_height, height);
    release.send(()).unwrap();
    assert_eq!(proof.await.unwrap(), expected_coinbase);

    // Caller-specific work must also refetch without replying with expired work or publishing it.
    let mut published = scheduler.published.clone();
    let _ = published.borrow_and_update();
    scheduler.chain_info.send_modify(|info| {
        info.expected_difficulty = CompactDifficulty::from(ExpandedDifficulty::from(U256::one()));
    });
    let mut response = scheduler.override_request();
    let stale = scheduler.proposal().await;
    scheduler.chain_info.send_modify(|info| {
        info.expected_difficulty = network.target_difficulty_limit().to_compact();
    });
    stale.respond(block::Hash([0; 32]));
    let replacement = scheduler.proposal().await;
    assert!(matches!(
        response.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    replacement.respond(block::Hash([0; 32]));
    let tip = scheduler.chain_info.borrow().tip_hash;
    let template = drive(&mut scheduler.templates, &scheduler.storage, tip, response)
        .await
        .unwrap()
        .unwrap();
    assert!(template.is_valid_for_tip(tip, &network, DateTime32::now()));
    assert!(!published.has_changed().unwrap());
}

#[tokio::test(start_paused = true)]
async fn historical_time_caps_do_not_trigger_immediate_rebuilds() {
    let _init_guard = zebra_test::init();
    let regtest = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    );
    for network in [Network::Mainnet, regtest, Network::new_default_testnet()] {
        let mut scheduler = Scheduler::new(network);
        scheduler.idle().await;
        assert_eq!(
            scheduler.checked.len(),
            1,
            "a full median-time cap cannot be extended"
        );
        assert!(scheduler.published.borrow().is_some());
    }
}

#[tokio::test(start_paused = true)]
async fn cancelled_expired_override_does_not_restart_proving() {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    let mut scheduler = Scheduler::new(network.clone());
    scheduler.chain_info.send_modify(|info| {
        info.min_time = info.max_time.saturating_sub(Duration32::from_seconds(100));
        info.expected_difficulty = network.target_difficulty_limit().to_compact();
    });
    scheduler.idle().await;
    let checked = scheduler.checked.len();
    let mut published = scheduler.published.clone();
    let _ = published.borrow_and_update();
    scheduler.chain_info.send_modify(|info| {
        info.expected_difficulty = CompactDifficulty::from(ExpandedDifficulty::from(U256::one()));
    });
    let response = scheduler.override_request();
    let stale = scheduler.proposal().await;
    drop(response);
    scheduler.chain_info.send_modify(|info| {
        info.expected_difficulty = network.target_difficulty_limit().to_compact();
    });
    stale.respond(block::Hash([0; 32]));
    scheduler.idle().await;
    assert_eq!(
        scheduler.checked.len(),
        checked,
        "cancelled work must not start another proposal"
    );
    assert!(!published.has_changed().unwrap());
}

#[tokio::test(start_paused = true)]
async fn changed_parent_publishes_before_retrying_private_work() {
    let _init_guard = zebra_test::init();
    let mut scheduler = Scheduler::new(Network::Mainnet);
    scheduler.idle().await;
    let mut response = scheduler.override_request();
    let held = scheduler.proposal().await;
    let changed_at = Instant::now();
    scheduler.chain_info.send_modify(|info| {
        info.tip_height = info.tip_height.next().unwrap();
        info.tip_hash = block::Hash([0xcd; 32]);
    });
    scheduler.templates.mark_changed();
    scheduler.poll();
    assert!(
        scheduler.templates.build.is_some(),
        "stale verification stays owned"
    );
    assert!(matches!(
        response.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    held.respond(block::Hash([0; 32]));
    let default = scheduler.proposal().await;
    assert_eq!(
        Instant::now(),
        changed_at,
        "new-parent work bypasses the debounce"
    );
    assert!(matches!(
        response.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    default.respond(block::Hash([0; 32]));
    scheduler.idle().await;
    let published = scheduler.published.borrow().clone().unwrap();
    assert_eq!(
        published.previous_block_hash(),
        scheduler.chain_info.borrow().tip_hash
    );
    tokio::time::advance(RETRY_DELAY).await;
    scheduler.idle().await;
    let private = response.await.unwrap().unwrap();
    assert_eq!(
        private.previous_block_hash(),
        published.previous_block_hash()
    );
    assert!(scheduler.published.borrow().as_ref().unwrap() == &published);
}

#[tokio::test(start_paused = true)]
async fn committed_parent_ahead_of_notification_does_not_loop_rebuilding() {
    let _init_guard = zebra_test::init();
    let mut scheduler = Scheduler::new(Network::Mainnet);
    let notified_tip = scheduler.chain_info.borrow().tip_hash;
    scheduler
        .chain_info
        .send_modify(|info| info.tip_hash = block::Hash([0xcd; 32]));
    let proposal = drive(
        &mut scheduler.templates,
        &scheduler.storage,
        notified_tip,
        scheduler.verifier.expect_request_that(|_| true),
    )
    .await;
    proposal.respond(block::Hash([0; 32]));
    let mut published = scheduler.published.clone();
    drive(
        &mut scheduler.templates,
        &scheduler.storage,
        notified_tip,
        published.changed(),
    )
    .await
    .unwrap();
    assert_eq!(
        published.borrow().as_ref().unwrap().previous_block_hash(),
        scheduler.chain_info.borrow().tip_hash
    );
    assert!(
        scheduler.templates.build.is_none(),
        "a lagging notification is not a new parent"
    );
    scheduler.poll();
    assert!(
        scheduler.templates.build.is_none(),
        "catching up to the committed parent needs no duplicate build"
    );
}

#[tokio::test(start_paused = true)]
async fn nsm_templates_do_not_precompute_without_parent_pools() {
    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(zebra_chain::parameters::testnet::RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu7: Some(20),
            ..Default::default()
        },
        nsm_reissuance_height: Some(Height(20)),
        ..Default::default()
    });
    let mut scheduler = Scheduler::new(network);
    scheduler
        .chain_info
        .send_modify(|info| info.tip_height = Height(20));
    scheduler.idle().await;
    assert!(scheduler.published.borrow().is_some());
    assert!(
        scheduler.templates.next_coinbase.is_none(),
        "the next parent's NSM balance is not known yet"
    );
}

#[tokio::test(start_paused = true)]
async fn removing_unselected_membership_refreshes_the_long_poll_id() {
    let _init_guard = zebra_test::init();
    let mut scheduler = Scheduler::new(Network::Mainnet);
    let mut tx = super::super::tests::admission::candidate();
    tx.fee_weight_ratio = 0.5;
    tx.unpaid_actions = u32::MAX;
    let id = tx.transaction.id;
    scheduler.storage.insert(tx, vec![], None).unwrap();
    scheduler.idle().await;
    let before = scheduler.published.borrow().clone().unwrap();
    assert!(
        before.transactions().is_empty(),
        "ZIP-317 excludes the unpaid transaction"
    );
    scheduler.storage.remove_exact(&[id].into_iter().collect());
    scheduler.templates.mark_changed();
    tokio::time::advance(CHANGE_DEBOUNCE).await;
    scheduler.idle().await;
    let after = scheduler.published.borrow().clone().unwrap();
    assert!(after.transactions().is_empty());
    assert_ne!(
        before.long_poll_id(),
        after.long_poll_id(),
        "long polling covers all verified membership, not just selection"
    );
}
