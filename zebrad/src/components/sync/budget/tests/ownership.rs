//! Cross-component ownership regressions for accepted checkpoint and semantic block commits.

use futures::future::{BoxFuture, FutureExt};

use super::*;

/// The maximum time for a real verifier/state scenario, including a bounded nonce search.
const OWNER_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A state service whose target commit is owned by a detached, gated task.
#[derive(Clone)]
struct GatedState<S> {
    state: S,
    target_height: Height,
    started: tokio::sync::mpsc::Sender<()>,
    released: tokio::sync::watch::Receiver<bool>,
}

/// Returns a gated state service with an explicitly `Send` response future.
fn gated_commit_state<S>(
    state: S,
    target_height: Height,
    started: tokio::sync::mpsc::Sender<()>,
    released: tokio::sync::watch::Receiver<bool>,
) -> GatedState<S> {
    GatedState {
        state,
        target_height,
        started,
        released,
    }
}

impl<S> Service<zs::Request> for GatedState<S>
where
    S: Service<zs::Request, Response = zs::Response, Error = zs::BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    type Response = zs::Response;
    type Error = zs::BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.state.poll_ready(context)
    }

    fn call(&mut self, request: zs::Request) -> Self::Future {
        let target = match &request {
            zs::Request::CommitCheckpointVerifiedBlock(block) => block.height == self.target_height,
            zs::Request::CommitSemanticallyVerifiedBlock(block) => {
                block.height == self.target_height
            }
            _ => false,
        };
        let mut state = self.state.clone();
        let started = self.started.clone();
        let mut released = self.released.clone();

        async move {
            if target {
                let (response_tx, response_rx) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    let _ = started.try_send(());
                    let result = async {
                        released.wait_for(|released| *released).await?;
                        state.ready().await?.call(request).await
                    }
                    .await;
                    let _ = response_tx.send(result);
                });
                return response_rx
                    .await
                    .map_err(|error| -> zs::BoxError { Box::new(error) })?;
            }

            state.ready().await?.call(request).await
        }
        .boxed()
    }
}

/// Wraps a real charge in the crate-neutral owner token used by the router.
fn retention_for(block: &Block, budget: &PipelineBudget) -> (zs::BlockRetentionToken, u64) {
    let bytes = downloaded_block_charge(block);
    let charge = budget.charge(bytes).expect("the pipeline is bounded");
    let retained_charge =
        usize::try_from(bytes).expect("the block charge was converted from usize");
    (
        zs::BlockRetentionToken::new(charge).with_retained_charge(retained_charge),
        bytes,
    )
}

/// Waits for the last accepted owner to drop its token.
async fn assert_charge_releases(budget: &PipelineBudget) {
    tokio::time::timeout(OWNER_TEST_TIMEOUT, async {
        while budget.charged_bytes() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the accepted owner releases its charge");
}

/// Returns a small Testnet whose checkpoint ranges complete with the Mainnet test vectors.
fn checkpoint_network() -> Network {
    ParametersBuilder::default()
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
        .expect("a permissive difficulty limit")
        .with_disable_pow(true)
        .with_genesis_hash(mainnet_block(0).hash())
        .expect("the generated genesis hash is valid")
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (Height(0), mainnet_block(0).hash()),
            (Height(5), mainnet_block(5).hash()),
            (Height(10), mainnet_block(10).hash()),
        ]))
        .expect("the generated checkpoint list is valid")
        .to_network()
        .expect("the generated test network is valid")
}

/// Caller cancellation does not release a checkpoint/state-owned body; its commit releases it.
#[tokio::test(flavor = "multi_thread")]
async fn checkpoint_owner_releases_after_caller_cancellation_and_commit() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(OWNER_TEST_TIMEOUT, async {
        let network = checkpoint_network();
        let (state, read_state, _tip, _tip_change) =
            zs::init(zs::Config::ephemeral(), &network, Height(10), 1_000).await;
        let state = ServiceBuilder::new().buffer(100).service(state);
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                mainnet_block(0).into(),
            ))
            .await
            .expect("genesis commits before the gated block");

        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);
        let (release, released) = tokio::sync::watch::channel(false);
        let gated = gated_commit_state(state, Height(1), started_tx, released);
        let (router, _transaction, _handles, max_checkpoint_height, _prepared) =
            zebra_consensus::router::init_test(
                zebra_consensus::Config::default(),
                &network,
                gated,
                read_state,
            )
            .await;
        assert!(max_checkpoint_height > Height(1));

        let budget = bounded(u64::MAX);
        let block = mainnet_block(1);
        let (retention, bytes) = retention_for(&block, &budget);
        let response = router
            .clone()
            .ready()
            .await
            .expect("the checkpoint router is ready")
            .call(zebra_consensus::Request::CommitWithRetention(
                block, retention,
            ));
        let verification = tokio::spawn(response);
        let mut other_verifications = Vec::new();
        for height in 2..=5 {
            let response = router
                .clone()
                .ready()
                .await
                .expect("the checkpoint router is ready for the range remainder")
                .call(zebra_consensus::Request::Commit(mainnet_block(height)));
            other_verifications.push(tokio::spawn(response));
        }

        tokio::time::timeout(OWNER_TEST_TIMEOUT, started_rx.recv())
            .await
            .expect("the gated checkpoint owner accepts the block");
        verification.abort();
        let _ = tokio::time::timeout(OWNER_TEST_TIMEOUT, verification).await;
        for verification in other_verifications {
            verification.abort();
            let _ = tokio::time::timeout(OWNER_TEST_TIMEOUT, verification).await;
        }
        assert_eq!(
            budget.charged_bytes(),
            bytes,
            "caller cancellation cannot release the accepted checkpoint body"
        );

        release
            .send(true)
            .expect("the test holds the release sender");
        assert_charge_releases(&budget).await;
    })
    .await
    .expect("the checkpoint ownership scenario finishes");
}

/// Replacing a queued checkpoint body evicts only its old token; commit releases the replacement.
#[tokio::test(flavor = "multi_thread")]
async fn checkpoint_replacement_evicts_only_the_replaced_charge() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(OWNER_TEST_TIMEOUT, async {
        let network = checkpoint_network();
        let (state, read_state, _tip, _tip_change) =
            zs::init(zs::Config::ephemeral(), &network, Height(10), 1_000).await;
        let state = ServiceBuilder::new().buffer(100).service(state);
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                mainnet_block(0).into(),
            ))
            .await
            .expect("genesis commits before duplicate queueing");
        let (router, _transaction, _handles, _max_checkpoint_height, _prepared) =
            zebra_consensus::router::init_test(
                zebra_consensus::Config::default(),
                &network,
                state,
                read_state,
            )
            .await;

        let budget = bounded(u64::MAX);
        let block = mainnet_block(1);
        let (first_retention, bytes) = retention_for(&block, &budget);
        let first_response = router
            .clone()
            .ready()
            .await
            .expect("the checkpoint router is ready for the first body")
            .call(zebra_consensus::Request::CommitWithRetention(
                block.clone(),
                first_retention,
            ));
        let first_verification = tokio::spawn(first_response);

        let second_bytes = bytes.saturating_add(1);
        let second_retention = zs::BlockRetentionToken::new(
            budget
                .charge(second_bytes)
                .expect("the replacement charge fits the bound"),
        );
        assert_eq!(budget.charged_bytes(), bytes.saturating_add(second_bytes));
        let second_response = router
            .clone()
            .ready()
            .await
            .expect("the checkpoint router is ready for the replacement")
            .call(zebra_consensus::Request::CommitWithRetention(
                block,
                second_retention,
            ));
        let second_verification = tokio::spawn(second_response);

        tokio::time::timeout(OWNER_TEST_TIMEOUT, async {
            while budget.charged_bytes() != second_bytes {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queue replacement evicts only the first charge");
        first_verification.abort();
        second_verification.abort();
        let _ = tokio::time::timeout(OWNER_TEST_TIMEOUT, first_verification).await;
        let _ = tokio::time::timeout(OWNER_TEST_TIMEOUT, second_verification).await;
        assert_eq!(
            budget.charged_bytes(),
            second_bytes,
            "caller cancellation retains the replacement owner"
        );

        let mut remaining = Vec::new();
        for height in 2..=5 {
            let response = router
                .clone()
                .ready()
                .await
                .expect("the checkpoint router is ready for the range remainder")
                .call(zebra_consensus::Request::Commit(mainnet_block(height)));
            remaining.push(tokio::spawn(response));
        }
        assert_charge_releases(&budget).await;
        for verification in remaining {
            verification.abort();
        }
    })
    .await
    .expect("the checkpoint replacement scenario finishes");
}

/// Caller cancellation does not release an inbound semantic/state-owned body either.
#[tokio::test(flavor = "multi_thread")]
async fn semantic_owner_releases_after_caller_cancellation_and_commit() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(OWNER_TEST_TIMEOUT, async {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let (state, read_state, _tip, _tip_change) =
            zs::init(zs::Config::ephemeral(), &network, Height(0), 0).await;
        let state = ServiceBuilder::new().buffer(100).service(state);
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                block::genesis::regtest_genesis_block().into(),
            ))
            .await
            .expect("Regtest genesis commits before the gated block");
        let candidate = semantic_candidate(&network, &read_state).await;

        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);
        let (release, released) = tokio::sync::watch::channel(false);
        let gated = gated_commit_state(state, Height(1), started_tx, released);
        let transactions =
            zebra_consensus::transaction::BlockTxVerifier::new(&network, gated.clone());
        let transactions = ServiceBuilder::new().buffer(10).service(transactions);
        let mut verifier =
            zebra_consensus::SemanticBlockVerifier::new(&network, gated, transactions);

        let budget = bounded(u64::MAX);
        let (retention, bytes) = retention_for(&candidate, &budget);
        let response = verifier
            .ready()
            .await
            .expect("the semantic verifier is ready")
            .call(zebra_consensus::Request::CommitWithRetention(
                candidate, retention,
            ));
        let verification = tokio::spawn(response);

        tokio::time::timeout(OWNER_TEST_TIMEOUT, started_rx.recv())
            .await
            .expect("the gated semantic owner accepts the block");
        verification.abort();
        let _ = tokio::time::timeout(OWNER_TEST_TIMEOUT, verification).await;
        assert_eq!(
            budget.charged_bytes(),
            bytes,
            "caller cancellation cannot release the accepted semantic body"
        );

        release
            .send(true)
            .expect("the test holds the release sender");
        assert_charge_releases(&budget).await;
    })
    .await
    .expect("the semantic ownership scenario finishes");
}

/// Builds a semantically valid Regtest block on the current tip.
async fn semantic_candidate(network: &Network, read_state: &zs::ReadStateService) -> Arc<Block> {
    let zs::ReadResponse::ChainInfo(info) = read_state
        .clone()
        .oneshot(zs::ReadRequest::ChainInfo)
        .await
        .expect("chain info is available for the candidate")
    else {
        panic!("chain info response")
    };
    let height = info
        .tip_height
        .next()
        .expect("the genesis has a child height");
    let transactions = vec![Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            zebra_chain::parameters::subsidy::block_subsidy(height, network)
                .expect("Regtest has a valid subsidy at the candidate height"),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        height,
    ))];
    let block = Block {
        header: Arc::new(block::Header {
            version: 4,
            previous_block_hash: info.tip_hash,
            merkle_root: transactions.iter().map(|tx| tx.hash()).collect(),
            commitment_bytes: [0; 32].into(),
            time: info.cur_time.into(),
            difficulty_threshold: info.expected_difficulty,
            nonce: [0; 32].into(),
            solution: equihash::Solution::Regtest([0; 36]),
        }),
        transactions,
    };
    let network = network.clone();

    tokio::task::spawn_blocking(move || seal_candidate(&network, block, info.chain_history_root))
        .await
        .expect("the bounded candidate sealing worker does not panic")
}

/// Seals a candidate with its chain-history commitment and a valid Regtest nonce.
fn seal_candidate(
    network: &Network,
    mut block: Block,
    history_root: Option<block::ChainHistoryMmrRootHash>,
) -> Arc<Block> {
    let height = block
        .coinbase_height()
        .expect("the candidate has a coinbase height");
    Arc::make_mut(&mut block.header).commitment_bytes =
        <[u8; 32]>::from(ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            &history_root.unwrap_or(block::CHAIN_HISTORY_ACTIVATION_RESERVED.into()),
            &block.auth_data_root(),
        ))
        .into();

    (0u32..100_000)
        .find(|nonce| {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&nonce.to_le_bytes());
            Arc::make_mut(&mut block.header).nonce = bytes.into();
            zebra_consensus::difficulty_is_valid(&block.header, network, &height, &block.hash())
                .is_ok()
        })
        .expect("the bounded Regtest nonce search finds a valid candidate");
    Arc::new(block)
}
