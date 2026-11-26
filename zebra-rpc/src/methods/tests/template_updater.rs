//! Tests of what makes the block template updater build and replace templates.

use std::{sync::Arc, time::Duration};

use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
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
use zebra_node_services::{
    mempool::{self, MempoolChange, MempoolTxSubscriber},
    BoxError,
};
use zebra_state::{GetBlockTemplateChainInfo, MiningCandidateChanges, ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use crate::{
    config::mining,
    methods::{
        tests::utils::fake_history_tree,
        types::{
            get_block_template::{
                constants::MEMPOOL_LONG_POLL_INTERVAL,
                precompute::{TemplateCache, TemplateChanges, MEMPOOL_REBUILD_PAUSE},
                BlockTemplateResponse, GetBlockTemplateParameters, GetBlockTemplateResponse,
            },
            long_poll::LongPollId,
        },
        RpcImpl, RpcServer,
    },
};

/// How long the tests wait for the updater to do something it should do promptly.
const PROMPT: Duration = Duration::from_secs(10);

/// How long the tests wait to check that the updater doesn't publish a template.
const QUIET: Duration = Duration::from_millis(500);

/// A block template updater on mock services, and the handles that drive them.
struct Updater {
    net: Network,

    /// The updater's template cache.
    cache: TemplateCache,

    /// The best tip that the mock state and the mock chain tip report.
    tip: watch::Sender<(Height, Hash)>,
    mock_tip_sender: MockChainTipSender,

    /// The early mining work that the mock state reports while its parent block is the best tip,
    /// and the hash of that parent block.
    candidate: watch::Sender<Option<(Hash, GetBlockTemplateChainInfo)>>,

    /// Publishes or withdraws early mining work on the updater's subscription.
    publish_candidate: Box<dyn Fn(Option<(Hash, GetBlockTemplateChainInfo)>) + Send + Sync>,

    /// The mempool's change channel.
    mempool_changes: broadcast::Sender<MempoolChange>,

    /// The start of each template build that read the mempool.
    builds: mpsc::UnboundedReceiver<Instant>,

    /// Lets the mock mempool answer one request, if it was started holding its answers.
    mempool_answers: mpsc::UnboundedSender<()>,

    /// A message for each early mining work request that the mock state gets, sent before it
    /// answers the request from the state it had when the request arrived.
    candidate_reads: mpsc::UnboundedReceiver<()>,

    /// Makes the mock state hold its answer to the next early mining work request until the
    /// receiver it is sent gets a message, or its sender is dropped.
    candidate_read_holds: mpsc::UnboundedSender<oneshot::Receiver<()>>,

    /// A message for each chain info request that the mock state gets, which is the first step
    /// of every template build that isn't on early mining work.
    chain_info_reads: mpsc::UnboundedReceiver<()>,

    /// Starts a `getblocktemplate` request that long polls on a template's long poll ID.
    long_poll: LongPoll,

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

/// Starts a `getblocktemplate` request that long polls on a long poll ID.
type LongPoll = Box<
    dyn Fn(LongPollId) -> JoinHandle<jsonrpsee::core::RpcResult<GetBlockTemplateResponse>>
        + Send
        + Sync,
>;

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

        let (candidate, candidate_receiver) =
            watch::channel::<Option<(Hash, GetBlockTemplateChainInfo)>>(None);
        let (candidate_changes, publish_candidate) = MiningCandidateChanges::new_for_tests();
        let (candidate_read_sender, candidate_reads) = mpsc::unbounded_channel();
        let (candidate_read_holds, mut held_candidate_reads) =
            mpsc::unbounded_channel::<oneshot::Receiver<()>>();
        let (chain_info_read_sender, chain_info_reads) = mpsc::unbounded_channel();
        let read_state_responder = {
            let net = net.clone();
            let tip = tip_receiver.clone();
            let mut read_state = read_state;
            let mut failed_subscriptions = failed_subscriptions;

            tokio::spawn(async move {
                loop {
                    let request = read_state.expect_request_that(|_| true).await;

                    // The request is answered from the state when it arrives, so a test that
                    // changes the state after it sees the request's message doesn't change the
                    // answer. A held request is answered from the state when it is released.
                    let state = || (*tip.borrow(), candidate_receiver.borrow().clone());
                    let mut answer_from = state();

                    match request.request() {
                        ReadRequest::MiningCandidate => {
                            let hold = held_candidate_reads.try_recv().ok();
                            let _ = candidate_read_sender.send(());

                            if let Some(release) = hold {
                                let _ = release.await;
                                answer_from = state();
                            }
                        }
                        ReadRequest::ChainInfo => {
                            let _ = chain_info_read_sender.send(());
                        }
                        _ => {}
                    }

                    let ((tip_height, tip_hash), candidate) = answer_from;

                    request.respond_with(|request| -> Result<ReadResponse, BoxError> {
                        Ok(match request {
                            ReadRequest::ChainInfo => {
                                ReadResponse::ChainInfo(chain_info(&net, tip_height, tip_hash))
                            }
                            ReadRequest::Tip => ReadResponse::Tip(Some((tip_height, tip_hash))),
                            // Like the state, only report early work on a child of the best tip.
                            ReadRequest::MiningCandidate => ReadResponse::MiningCandidate(
                                candidate
                                    .clone()
                                    .filter(|(parent_hash, _)| *parent_hash == tip_hash)
                                    .map(|(_, candidate)| candidate),
                            ),
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

        let (mempool_changes, _) = broadcast::channel(16);
        let cache = rpc
            .gbt
            .template_cache()
            .expect("the miner params were not overridden")
            .clone();

        let long_poll: LongPoll = {
            let rpc = rpc.clone();
            Box::new(move |long_poll_id| {
                let rpc = rpc.clone();
                tokio::spawn(async move {
                    rpc.get_block_template(Some(GetBlockTemplateParameters {
                        long_poll_id: Some(long_poll_id),
                        ..Default::default()
                    }))
                    .await
                })
            })
        };

        let updater = Self {
            net,
            cache,
            tip,
            mock_tip_sender,
            candidate,
            publish_candidate: Box::new(publish_candidate),
            mempool_changes: mempool_changes.clone(),
            builds,
            mempool_answers,
            candidate_reads,
            candidate_read_holds,
            chain_info_reads,
            long_poll,
            tasks: vec![read_state_responder, mempool_responder],
        };
        updater.set_tip(tip_height, tip_hash);

        let task = rpc
            .spawn_block_template_updater(&MempoolTxSubscriber::new(mempool_changes))
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

    /// Commits the block `block`, a child of the best tip, like the state does: it changes its best
    /// tip, and then the chain tip changes once.
    ///
    /// Only the mock chain tip's hash changes, because each field the mock changes wakes the
    /// updater, and the updater only needs the hash to see the commit.
    fn commit(&self, block: (Height, Hash)) {
        self.tip.send_replace(block);
        self.mock_tip_sender.send_best_tip_hash(block.1);
    }

    /// Publishes early mining work on the block `candidate`, a child of the best tip, or
    /// withdraws the work.
    ///
    /// The work is the chain info that the mock state reports once the block is committed.
    fn set_candidate(&self, candidate: Option<(Height, Hash)>) {
        let (_, parent_hash) = self.best_tip();
        let candidate =
            candidate.map(|(height, hash)| (parent_hash, chain_info(&self.net, height, hash)));

        self.candidate.send_replace(candidate.clone());
        (self.publish_candidate)(candidate);
    }

    /// Wakes the updater with a chain tip change that keeps the best tip's hash.
    fn wake_updater(&self) {
        self.mock_tip_sender
            .send_best_tip_block_time(chrono::Utc::now());
    }

    /// Waits for the next early mining work request that the mock state gets.
    ///
    /// The updater only reads the early work again after it has finished building the template
    /// on the work it read before, so this is a point where that build is finished.
    async fn candidate_read(&mut self) {
        tokio::time::timeout(PROMPT, self.candidate_reads.recv())
            .await
            .expect("the updater reads the early mining work")
            .expect("the read state responder is running");
    }

    /// Makes the mock state hold its answer to the next early mining work request, and returns
    /// the sender that releases it. The mock state answers from its state when it is released.
    fn hold_candidate_read(&self) -> oneshot::Sender<()> {
        let (release, held) = oneshot::channel();
        self.candidate_read_holds
            .send(held)
            .expect("the read state responder is running");

        release
    }

    /// Returns the number of chain info requests the mock state has got since the last call.
    fn chain_info_reads(&mut self) -> usize {
        std::iter::from_fn(|| self.chain_info_reads.try_recv().ok()).count()
    }

    /// Forgets the requests the mock state has got so far.
    fn forget_reads(&mut self) {
        while self.candidate_reads.try_recv().is_ok() {}
        self.chain_info_reads();
    }

    /// Starts a `getblocktemplate` request that long polls on `template`.
    fn long_poll(
        &self,
        template: &BlockTemplateResponse,
    ) -> JoinHandle<jsonrpsee::core::RpcResult<GetBlockTemplateResponse>> {
        (self.long_poll)(template.long_poll_id)
    }

    /// Checks that `templates` doesn't see a template published within [`QUIET`], that the cache
    /// still has `tip_template`, and that it has no template on the block `block_hash`.
    async fn assert_nothing_published(
        &self,
        templates: &mut TemplateChanges,
        tip_template: &Arc<BlockTemplateResponse>,
        block_hash: Hash,
    ) {
        let (_, tip_hash) = self.best_tip();

        assert!(
            tokio::time::timeout(QUIET, templates.changed())
                .await
                .is_err(),
            "no template is published, so long polls aren't woken"
        );
        assert!(
            Arc::ptr_eq(&self.template(tip_hash), tip_template),
            "the cache still has the chain tip's template"
        );
        assert!(
            !self.has_template_on(block_hash),
            "the cache has no template on the block"
        );
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

    /// Starts an updater whose mock mempool holds its answers, and waits until it has published
    /// a template with mempool transactions on the chain tip. Returns the updater, which is then
    /// waiting for a change, and that template.
    async fn start_with_tip_template() -> (Self, Arc<BlockTemplateResponse>) {
        let mut updater = Self::start_holding_mempool_answers().await;
        let (_, tip_hash) = updater.best_tip();

        // The updater publishes a coinbase-only template, and then builds one from the mempool,
        // which replaces it.
        updater.template_on(tip_hash).await;
        updater.build().await;
        let coinbase_only_template = updater.template(tip_hash);
        updater.answer_mempool();
        let tip_template = updater
            .template_after(tip_hash, &coinbase_only_template)
            .await;

        updater.forget_reads();

        (updater, tip_template)
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

/// The updater builds a coinbase-only template on a block that is still being verified, but
/// doesn't publish it, so neither the cache nor a long polling miner gets work on the block. Once
/// the block is committed, the updater publishes that template without building one, and it is
/// the template that a build on the committed block makes.
#[tokio::test(flavor = "multi_thread")]
async fn updater_publishes_a_mining_candidates_template_once_its_block_is_committed() {
    let _init_guard = zebra_test::init();

    let (mut updater, tip_template) = Updater::start_with_tip_template().await;
    let (tip_height, _) = updater.best_tip();
    let mut templates = updater.cache.subscribe();
    let long_poll = updater.long_poll(&tip_template);

    // A block with a valid proof of work arrives.
    let candidate = (
        tip_height.next().expect("the height is valid"),
        Hash([0x22; 32]),
    );
    updater.set_candidate(Some(candidate));
    updater.candidate_read().await;

    // A chain tip change that keeps the tip makes the updater check that the early work is still
    // there, after it has built the template on the block, and then read the work again at the
    // start of its loop. It then waits for the block.
    updater.wake_updater();
    updater.candidate_read().await;
    updater.candidate_read().await;
    updater
        .assert_nothing_published(&mut templates, &tip_template, candidate.1)
        .await;
    assert!(
        !long_poll.is_finished(),
        "a long polling miner isn't sent work on a block that isn't committed"
    );
    assert!(
        !updater.has_built(),
        "work on a block that is still being verified is coinbase-only"
    );

    // The block is committed: the state changes the chain tip, and then withdraws the work. The
    // template is published before the withdrawal, without waiting for a refresh to be due.
    let committed = Instant::now();
    updater.commit(candidate);
    updater.template_on(candidate.1).await;
    assert!(
        committed.elapsed() < Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL),
        "the template was published {:?} after the commit",
        committed.elapsed()
    );
    updater.set_candidate(None);
    let early_template = updater.template(candidate.1);

    let long_poll = tokio::time::timeout(PROMPT, long_poll)
        .await
        .expect("the long polling miner gets work on the committed block")
        .expect("the long poll task doesn't panic")
        .expect("the long poll succeeds")
        .try_into_template()
        .expect("the long poll is in template mode");
    assert_eq!(long_poll.previous_block_hash, candidate.1);

    // Then the updater builds a template with mempool transactions on the block. Its state read
    // is the only one since the commit, so the template on the block wasn't built after it.
    updater.build().await;
    assert_eq!(
        updater.chain_info_reads(),
        1,
        "the template on the block is published without being built again"
    );

    // The mempool is empty, so that build makes the same template.
    updater.answer_mempool();
    let full_template = updater.template_after(candidate.1, &early_template).await;
    assert!(
        *full_template == *early_template,
        "the template on early work is the template on the committed block"
    );
}

/// When the state withdraws its early work on a block, because the block is rejected or takes too
/// long to verify, the updater publishes nothing, and miners keep the chain tip's template until
/// it is due for a refresh.
#[tokio::test(flavor = "multi_thread")]
async fn updater_publishes_nothing_for_a_withdrawn_mining_candidate() {
    let _init_guard = zebra_test::init();

    let (mut updater, tip_template) = Updater::start_with_tip_template().await;
    let (tip_height, _) = updater.best_tip();
    let mut templates = updater.cache.subscribe();

    let candidate = (
        tip_height.next().expect("the height is valid"),
        Hash([0x22; 32]),
    );
    updater.set_candidate(Some(candidate));
    updater.candidate_read().await;

    // The updater reads the early work again when it is withdrawn.
    let withdrawn = Instant::now();
    updater.set_candidate(None);
    updater.candidate_read().await;
    updater
        .assert_nothing_published(&mut templates, &tip_template, candidate.1)
        .await;

    // The block is never committed, and the updater refreshes the tip's template when it is due,
    // rather than straight away, so it isn't busy with that build if the block is committed
    // late. The mock mempool holds the build.
    let refresh = updater.build().await;
    assert!(
        refresh - withdrawn >= Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL),
        "the template was refreshed {:?} after the withdrawal",
        refresh - withdrawn
    );
    updater
        .assert_nothing_published(&mut templates, &tip_template, candidate.1)
        .await;
}

/// A block that takes too long to verify can still be committed after the state withdraws its
/// early work. The updater keeps the block's template, and publishes it without building one when
/// the block is committed.
#[tokio::test(flavor = "multi_thread")]
async fn updater_publishes_the_template_of_a_block_committed_after_its_work_is_withdrawn() {
    let _init_guard = zebra_test::init();

    let (mut updater, tip_template) = Updater::start_with_tip_template().await;
    let (tip_height, _) = updater.best_tip();
    let mut templates = updater.cache.subscribe();

    let candidate = (
        tip_height.next().expect("the height is valid"),
        Hash([0x22; 32]),
    );
    updater.set_candidate(Some(candidate));
    updater.candidate_read().await;
    updater.set_candidate(None);
    updater.candidate_read().await;
    updater
        .assert_nothing_published(&mut templates, &tip_template, candidate.1)
        .await;

    // The mock mempool holds its answers, so the updater can only publish a template on the
    // block if it isn't building the tip's template when the block is committed.
    updater.commit(candidate);
    updater.template_on(candidate.1).await;

    updater.build().await;
    assert_eq!(
        updater.chain_info_reads(),
        1,
        "the template on the block is published without being built again"
    );
}

/// Early work on another block replaces the kept template, and the updater publishes the template
/// on the block that is committed without building one.
#[tokio::test(flavor = "multi_thread")]
async fn updater_replaces_a_kept_template_with_one_on_newer_early_work() {
    let _init_guard = zebra_test::init();

    let (mut updater, tip_template) = Updater::start_with_tip_template().await;
    let (tip_height, _) = updater.best_tip();
    let mut templates = updater.cache.subscribe();

    let height = tip_height.next().expect("the height is valid");
    let (first, second) = ((height, Hash([0x22; 32])), (height, Hash([0x33; 32])));

    updater.set_candidate(Some(first));
    updater.candidate_read().await;

    // The updater reads the early work again when it changes, and then builds a template on the
    // second block.
    updater.set_candidate(Some(second));
    updater.candidate_read().await;
    updater
        .assert_nothing_published(&mut templates, &tip_template, first.1)
        .await;
    updater.candidate_read().await;

    updater.commit(second);
    updater.set_candidate(None);
    updater.template_on(second.1).await;

    updater.build().await;
    assert_eq!(
        updater.chain_info_reads(),
        1,
        "the template on the second block is published without being built again"
    );
}

/// The updater drops the kept template when another block becomes the chain tip, so a template on
/// the block is built again if the block becomes the chain tip later.
#[tokio::test(flavor = "multi_thread")]
async fn updater_drops_a_kept_template_when_another_block_is_committed() {
    let _init_guard = zebra_test::init();

    let (mut updater, tip_template) = Updater::start_with_tip_template().await;
    let (tip_height, _) = updater.best_tip();
    let mut templates = updater.cache.subscribe();

    let height = tip_height.next().expect("the height is valid");
    let (candidate, other_block) = ((height, Hash([0x22; 32])), (height, Hash([0x44; 32])));

    updater.set_candidate(Some(candidate));
    updater.candidate_read().await;
    updater.set_candidate(None);
    updater.candidate_read().await;
    updater
        .assert_nothing_published(&mut templates, &tip_template, candidate.1)
        .await;

    // Another block is committed. The mock mempool holds the build on it.
    updater.set_tip(other_block.0, other_block.1);
    updater.template_on(other_block.1).await;
    updater.build().await;
    updater.forget_reads();

    // The chain reorganizes onto the block of the dropped template.
    updater.set_tip(candidate.0, candidate.1);
    updater.answer_mempool();
    updater.template_on(candidate.1).await;

    updater.build().await;
    assert_eq!(
        updater.chain_info_reads(),
        2,
        "the template on the block is built again, and then with mempool transactions"
    );
}

/// The updater keeps the template on a block whose early work was withdrawn, while the chain tip
/// is still the block's parent, even after it has refreshed the chain tip's template. If the
/// state commits the block while the updater reads the early work at the start of its loop, that
/// read finds no work, and the updater publishes the kept template without building one.
#[tokio::test(flavor = "multi_thread")]
async fn updater_publishes_a_kept_template_when_its_block_is_committed_during_a_read_of_early_work()
{
    let _init_guard = zebra_test::init();

    let (mut updater, tip_template) = Updater::start_with_tip_template().await;
    let (tip_height, tip_hash) = updater.best_tip();

    let candidate = (
        tip_height.next().expect("the height is valid"),
        Hash([0x22; 32]),
    );
    updater.set_candidate(Some(candidate));
    updater.candidate_read().await;
    updater.set_candidate(None);
    updater.candidate_read().await;

    // A chain tip change that keeps the tip ends the wait after the withdrawal, and the updater
    // refreshes the chain tip's template.
    updater.wake_updater();
    updater.build().await;
    updater.answer_mempool();
    updater.template_after(tip_hash, &tip_template).await;
    updater.forget_reads();

    // The updater wakes up again, and the block is committed while the mock state holds the
    // updater's read of the early work.
    let release = updater.hold_candidate_read();
    updater.wake_updater();
    updater.candidate_read().await;
    updater.commit(candidate);
    release
        .send(())
        .expect("the read state responder holds the read");

    updater.template_on(candidate.1).await;
    updater.build().await;
    assert_eq!(
        updater.chain_info_reads(),
        1,
        "the template on the block is published without being built again"
    );
}

/// When the chain tip changes while the updater builds a template, and the state is already
/// verifying a block on the new tip when the updater sees the change, the updater builds the
/// template on that block first. If the state then withdraws its work on the block, the updater
/// builds the new tip's template straight away, rather than when a refresh would be due, because
/// miners have no template on that tip.
#[tokio::test(flavor = "multi_thread")]
async fn updater_builds_a_new_tips_template_straight_away_when_early_work_on_its_child_is_withdrawn(
) {
    let _init_guard = zebra_test::init();

    let (mut updater, _tip_template) = Updater::start_with_tip_template().await;
    let (tip_height, _) = updater.best_tip();

    // The updater refreshes the chain tip's template, and the mock mempool holds that build.
    updater.wake_updater();
    updater.build().await;
    updater.forget_reads();

    // A new block is committed, and the state starts verifying a block on top of it.
    let new_tip = (
        tip_height.next().expect("the height is valid"),
        Hash([0x33; 32]),
    );
    let candidate = (
        new_tip.0.next().expect("the height is valid"),
        Hash([0x22; 32]),
    );
    updater.set_tip(new_tip.0, new_tip.1);
    updater.set_candidate(Some(candidate));

    // The refresh isn't on the new tip, so the updater discards it, and then reads the early work.
    updater.answer_mempool();
    updater.candidate_read().await;

    let withdrawn = Instant::now();
    updater.set_candidate(None);
    updater.template_on(new_tip.1).await;
    assert!(
        withdrawn.elapsed() < Duration::from_secs(MEMPOOL_LONG_POLL_INTERVAL),
        "the template on the new tip was published {:?} after the withdrawal",
        withdrawn.elapsed()
    );
}

/// The updater rebuilds its template when the mempool verifies a transaction, before the
/// periodic refresh, and at most once per pause however many transactions arrive. A tip change
/// during the pause still gets a template right away.
#[tokio::test(flavor = "multi_thread")]
async fn updater_rebuilds_on_mempool_additions() {
    let _init_guard = zebra_test::init();

    // The periodic refresh is 5 seconds after a build, so a rebuild this soon after one is the
    // mempool's.
    let before_refresh = Duration::from_secs(3);
    // A pause is measured from the end of a build, and the builds here take a few milliseconds.
    let pause = MEMPOOL_REBUILD_PAUSE - Duration::from_millis(100);
    let add_transaction = |updater: &Updater| {
        updater
            .mempool_changes
            .send(MempoolChange::added(Default::default()))
            .expect("the updater is subscribed");
    };

    let mut updater = Updater::start(0).await;
    let (tip_height, tip_hash) = updater.best_tip();
    updater.template_on(tip_hash).await;
    let first = updater.build().await;

    // A new transaction rebuilds the template after the pause.
    add_transaction(&updater);
    let second = updater.build().await;
    assert!(
        (pause..before_refresh).contains(&(second - first)),
        "the template was rebuilt after {:?}",
        second - first
    );

    // A burst of transactions rebuilds it once.
    for _ in 0..5 {
        add_transaction(&updater);
    }
    let third = updater.build().await;
    assert!(
        (pause..before_refresh).contains(&(third - second)),
        "the template was rebuilt after {:?}",
        third - second
    );
    tokio::time::sleep(pause).await;
    assert!(!updater.has_built(), "a burst is one rebuild");

    // A transaction and then a tip change, both during a pause: the new tip doesn't wait.
    add_transaction(&updater);
    let fourth = updater.build().await;
    let next_tip = (
        tip_height.next().expect("the height is valid"),
        Hash([0x33; 32]),
    );
    updater.set_tip(next_tip.0, next_tip.1);
    updater.template_on(next_tip.1).await;
    let on_next_tip = updater.build().await;
    assert!(
        on_next_tip - fourth < pause,
        "the template on the new tip was built after {:?}",
        on_next_tip - fourth
    );
}
