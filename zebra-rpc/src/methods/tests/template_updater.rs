//! Tests of what makes the block template updater build and replace templates.

use std::{sync::Arc, time::Duration};

use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};
use tower::buffer::Buffer;

use zcash_address::{ToAddress, ZcashAddress};
use zcash_protocol::consensus::NetworkType;

use zebra_chain::{
    block::{Hash, Height},
    chain_sync_status::MockSyncStatus,
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::{Network, NetworkKind, NetworkUpgrade},
    serialization::DateTime32,
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_node_services::{mempool, BoxError};
use zebra_state::{GetBlockTemplateChainInfo, MiningCandidateChanges, ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use crate::{
    config::mining,
    methods::{
        tests::utils::fake_history_tree,
        types::get_block_template::{precompute::TemplateCache, BlockTemplateResponse},
        RpcImpl,
    },
};

/// How long the tests wait for the updater to do something it should do promptly.
const PROMPT: Duration = Duration::from_secs(10);

/// A block template updater on mock services, and the handles that drive them.
struct Updater {
    net: Network,

    /// The updater's template cache.
    cache: TemplateCache,

    /// The best tip that the mock state and the mock chain tip report.
    tip: watch::Sender<(Height, Hash)>,
    mock_tip_sender: MockChainTipSender,

    /// The early mining work that the mock state reports.
    candidate: watch::Sender<Option<GetBlockTemplateChainInfo>>,

    /// Publishes or withdraws early mining work on the updater's subscription.
    publish_candidate: Box<dyn Fn(Option<(Hash, GetBlockTemplateChainInfo)>) + Send + Sync>,

    /// The start of each template build that read the mempool.
    builds: mpsc::UnboundedReceiver<Instant>,

    /// Lets the mock mempool answer one request, if it was started holding its answers.
    mempool_answers: mpsc::UnboundedSender<()>,

    /// The updater and the mock responders.
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Updater {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Returns the state's information for a block template on the tip `(tip_height, tip_hash)`.
fn chain_info(net: &Network, tip_height: Height, tip_hash: Hash) -> GetBlockTemplateChainInfo {
    GetBlockTemplateChainInfo {
        expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
        tip_height,
        tip_hash,
        cur_time: DateTime32::from(1654008617),
        min_time: DateTime32::from(1654008606),
        max_time: DateTime32::from(1654008728),
        chain_history_root: fake_history_tree(net).hash(),
        chain_value_pools: Default::default(),
    }
}

impl Updater {
    /// Starts an updater on a Mainnet tip. The mock state fails the updater's first
    /// `failed_subscriptions` subscriptions to early mining work.
    async fn start(failed_subscriptions: usize) -> Self {
        Self::start_with(failed_subscriptions, false).await
    }

    /// Starts an updater whose mock mempool holds each answer until the test calls
    /// [`Self::answer_mempool`].
    ///
    /// A template build that reads the mempool waits for that answer, and the updater does
    /// nothing else in the meantime. So a test can read the cache while the updater can't
    /// publish a template.
    async fn start_holding_mempool_answers() -> Self {
        Self::start_with(0, true).await
    }

    async fn start_with(failed_subscriptions: usize, hold_mempool_answers: bool) -> Self {
        let net = Network::Mainnet;

        // The updater only queries the mocks every few seconds, so the responders wait longer
        // than the default request delay.
        let request_delay = Duration::from_secs(30);
        let mempool: MockService<_, _, _, BoxError> = MockService::build()
            .with_max_request_delay(request_delay)
            .for_unit_tests();
        let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let read_state: MockService<_, _, _, BoxError> = MockService::build()
            .with_max_request_delay(request_delay)
            .for_unit_tests();
        let block_verifier_router: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();

        let mut mock_sync_status = MockSyncStatus::default();
        mock_sync_status.set_is_close_to_tip(true);

        let mining_conf = mining::Config {
            miner_address: Some(ZcashAddress::from_transparent_p2pkh(
                NetworkType::from(NetworkKind::from(&net)),
                [0x7e; 20],
            )),
            extra_coinbase_data: None,
            miner_memo: None,
            internal_miner: false,
        };

        let tip_height = NetworkUpgrade::Nu5
            .activation_height(&net)
            .expect("NU5 has an activation height");
        let tip_hash = Hash([0x11; 32]);
        let (mock_tip, mock_tip_sender) = MockChainTip::new();
        let (tip, tip_receiver) = watch::channel((tip_height, tip_hash));

        let (_last_event, last_event_receiver) = watch::channel(None);
        let (rpc, _queue) = RpcImpl::new(
            net.clone(),
            mining_conf,
            Default::default(),
            "0.0.1",
            "RPC test",
            Buffer::new(mempool.clone(), 1),
            state,
            Buffer::new(read_state.clone(), 1),
            block_verifier_router,
            mock_sync_status,
            mock_tip,
            MockAddressBookPeers::default(),
            last_event_receiver,
            None,
        );

        let (candidate, candidate_receiver) = watch::channel(None);
        let (candidate_changes, publish_candidate) = MiningCandidateChanges::new_for_tests();
        let read_state_responder = {
            let net = net.clone();
            let tip = tip_receiver.clone();
            let mut read_state = read_state;
            let mut failed_subscriptions = failed_subscriptions;

            tokio::spawn(async move {
                loop {
                    let request = read_state.expect_request_that(|_| true).await;
                    let (tip_height, tip_hash) = *tip.borrow();

                    request.respond_with(|request| -> Result<ReadResponse, BoxError> {
                        Ok(match request {
                            ReadRequest::ChainInfo => {
                                ReadResponse::ChainInfo(chain_info(&net, tip_height, tip_hash))
                            }
                            ReadRequest::Tip => ReadResponse::Tip(Some((tip_height, tip_hash))),
                            ReadRequest::MiningCandidate => {
                                ReadResponse::MiningCandidate(candidate_receiver.borrow().clone())
                            }
                            ReadRequest::MiningCandidateChanges if failed_subscriptions > 0 => {
                                failed_subscriptions -= 1;
                                return Err("the state is unavailable".into());
                            }
                            ReadRequest::MiningCandidateChanges => {
                                ReadResponse::MiningCandidateChanges(candidate_changes.clone())
                            }
                            other => panic!("unexpected read state request: {other:?}"),
                        })
                    });
                }
            })
        };

        let (build_sender, builds) = mpsc::unbounded_channel();
        let (mempool_answers, mut held_answers) = mpsc::unbounded_channel();
        let mempool_responder = {
            let tip = tip_receiver;
            let mut mempool = mempool;

            tokio::spawn(async move {
                loop {
                    let request = mempool
                        .expect_request(mempool::Request::FullTransactions)
                        .await;
                    let _ = build_sender.send(Instant::now());

                    if hold_mempool_answers && held_answers.recv().await.is_none() {
                        return;
                    }
                    let (_, tip_hash) = *tip.borrow();

                    request.respond(mempool::Response::FullTransactions {
                        transactions: vec![],
                        transaction_dependencies: Default::default(),
                        last_seen_tip_hash: tip_hash,
                    });
                }
            })
        };

        let cache = rpc
            .gbt
            .template_cache()
            .expect("the miner params were not overridden")
            .clone();

        let updater = Self {
            net,
            cache,
            tip,
            mock_tip_sender,
            candidate,
            publish_candidate: Box::new(publish_candidate),
            builds,
            mempool_answers,
            tasks: vec![read_state_responder, mempool_responder],
        };
        updater.set_tip(tip_height, tip_hash);

        let task = rpc
            .spawn_block_template_updater()
            .expect("mining is configured");
        let mut updater = updater;
        updater.tasks.push(task);

        updater
    }

    /// Returns the best tip.
    fn best_tip(&self) -> (Height, Hash) {
        *self.tip.borrow()
    }

    /// Changes the best tip that the state and the chain tip report.
    ///
    /// The mock chain tip changes one field at a time, and each change wakes the updater. The
    /// updater reads the tip's hash when it wakes, so the hash changes first.
    fn set_tip(&self, tip_height: Height, tip_hash: Hash) {
        self.tip.send_replace((tip_height, tip_hash));
        self.mock_tip_sender.send_best_tip_hash(tip_hash);
        self.mock_tip_sender.send_best_tip_height(tip_height);
        self.mock_tip_sender
            .send_best_tip_block_time(chrono::Utc::now());
    }

    /// Publishes early mining work on the block `candidate`, a child of the best tip, or
    /// withdraws the work.
    fn set_candidate(&self, candidate: Option<(Height, Hash)>) {
        let (_, parent_hash) = self.best_tip();
        let candidate = candidate.map(|(height, hash)| chain_info(&self.net, height, hash));

        self.candidate.send_replace(candidate.clone());
        (self.publish_candidate)(candidate.map(|candidate| (parent_hash, candidate)));
    }

    /// Returns `true` if the cache has a template on the block `tip_hash`.
    fn has_template_on(&self, tip_hash: Hash) -> bool {
        self.cache
            .template_for_tip(tip_hash, &self.net, DateTime32::now())
            .is_some()
    }

    /// Returns the cache's template on the block `tip_hash`.
    fn template(&self, tip_hash: Hash) -> Arc<BlockTemplateResponse> {
        self.cache
            .template_for_tip(tip_hash, &self.net, DateTime32::now())
            .expect("the cache has a template on the block")
    }

    /// Waits until the cache has a template on the block `tip_hash`.
    async fn template_on(&self, tip_hash: Hash) {
        tokio::time::timeout(PROMPT, async {
            let mut templates = self.cache.subscribe();
            while !self.has_template_on(tip_hash) {
                templates.changed().await;
            }
        })
        .await
        .expect("the updater publishes a template on the block");
    }

    /// Waits until the cache has a template on the block `tip_hash` that isn't `previous`, and
    /// returns it.
    async fn template_after(
        &self,
        tip_hash: Hash,
        previous: &Arc<BlockTemplateResponse>,
    ) -> Arc<BlockTemplateResponse> {
        tokio::time::timeout(PROMPT, async {
            let mut templates = self.cache.subscribe();
            loop {
                let template = self.template(tip_hash);
                if !Arc::ptr_eq(&template, previous) {
                    return template;
                }
                templates.changed().await;
            }
        })
        .await
        .expect("the updater publishes another template on the block")
    }

    /// Waits for the next template build that reads the mempool, and returns when it started.
    async fn build(&mut self) -> Instant {
        tokio::time::timeout(PROMPT, self.builds.recv())
            .await
            .expect("the updater builds a template with mempool transactions")
            .expect("the mempool responder is running")
    }

    /// Lets the mock mempool answer the request it is holding, or the next one.
    fn answer_mempool(&self) {
        self.mempool_answers
            .send(())
            .expect("the mempool responder is running");
    }

    /// Returns `true` if a template build has read the mempool since the last call to
    /// [`Self::build`] or to this method.
    fn has_built(&mut self) -> bool {
        self.builds.try_recv().is_ok()
    }
}

/// The updater keeps trying to subscribe to early mining work, and precomputes templates once the
/// state answers.
#[tokio::test(flavor = "multi_thread")]
async fn updater_retries_its_subscription_to_early_mining_work() {
    let _init_guard = zebra_test::init();

    let mut updater = Updater::start(2).await;
    let (_, tip_hash) = updater.best_tip();

    updater.template_on(tip_hash).await;
    updater.build().await;
}

/// The updater publishes coinbase-only work on a block that is still being verified. It goes
/// back to work on the chain tip when that block is withdrawn, without a tip change, and it
/// builds ordinary work on the block once the block is the chain tip.
#[tokio::test(flavor = "multi_thread")]
async fn updater_follows_mining_candidates() {
    let _init_guard = zebra_test::init();

    let mut updater = Updater::start(0).await;
    let (tip_height, tip_hash) = updater.best_tip();
    updater.template_on(tip_hash).await;
    updater.build().await;

    // A block with a valid proof of work arrives. Work on it doesn't read the mempool.
    let candidate = (
        tip_height.next().expect("the height is valid"),
        Hash([0x22; 32]),
    );
    updater.set_candidate(Some(candidate));
    updater.template_on(candidate.1).await;
    assert!(!updater.has_template_on(tip_hash));
    assert!(
        !updater.has_built(),
        "work on a block that is still being verified is coinbase-only"
    );

    // The block is rejected, so its work is withdrawn.
    updater.set_candidate(None);
    updater.template_on(tip_hash).await;
    assert!(!updater.has_template_on(candidate.1));
    updater.build().await;

    // The block arrives again, and this time it is committed.
    updater.set_candidate(Some(candidate));
    updater.template_on(candidate.1).await;
    assert!(!updater.has_built());

    updater.set_tip(candidate.0, candidate.1);
    updater.set_candidate(None);
    updater.build().await;
    updater.template_on(candidate.1).await;
    assert!(
        !updater.has_template_on(tip_hash),
        "a committed block's work isn't replaced with work on its parent"
    );
}

/// When early mining work is withdrawn before its block is committed, the updater publishes the
/// chain tip's template again straight away, without building one. If the block is committed
/// after that, it publishes the block's template again.
#[tokio::test(flavor = "multi_thread")]
async fn updater_restores_the_templates_that_a_withdrawal_replaces() {
    let _init_guard = zebra_test::init();

    let mut updater = Updater::start_holding_mempool_answers().await;
    let (tip_height, tip_hash) = updater.best_tip();

    // The updater publishes a coinbase-only template, and then builds one from the mempool,
    // which replaces it. After that build it waits for a few seconds.
    updater.template_on(tip_hash).await;
    updater.build().await;
    let coinbase_only_template = updater.template(tip_hash);
    updater.answer_mempool();
    let tip_template = updater
        .template_after(tip_hash, &coinbase_only_template)
        .await;

    let candidate = (
        tip_height.next().expect("the height is valid"),
        Hash([0x22; 32]),
    );
    updater.set_candidate(Some(candidate));
    updater.template_on(candidate.1).await;
    let candidate_template = updater.template(candidate.1);

    // The block takes too long to verify, so its work is withdrawn. The mempool holds its
    // answers, so a template that the updater built would be the coinbase-only one.
    updater.set_candidate(None);
    updater.template_on(tip_hash).await;
    assert!(
        Arc::ptr_eq(&updater.template(tip_hash), &tip_template),
        "the chain tip's template is published again, not built again"
    );

    // The block is committed after all.
    updater.set_tip(candidate.0, candidate.1);
    updater.template_on(candidate.1).await;
    assert!(
        Arc::ptr_eq(&updater.template(candidate.1), &candidate_template),
        "the block's template is published again, not built again"
    );

    // Then the updater builds ordinary work on the block.
    updater.build().await;
    updater.answer_mempool();
    updater
        .template_after(candidate.1, &candidate_template)
        .await;
}
