//! Proposal admission, response deadlines, and live-context preflight regressions.

use std::{
    future::{ready, Ready},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};

use jsonrpsee::core::RpcResult;
use tokio::sync::Semaphore;
use tower::{buffer::Buffer, Service};

use zebra_chain::{
    block::{Block, Hash, Height},
    chain_sync_status::MockSyncStatus,
    chain_tip::NoChainTip,
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters},
        Network,
    },
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_consensus::{router::service_trait::BlockVerifierService, Request, VerifyBlockError};
use zebra_node_services::BoxError;
use zebra_state::{GetBlockTemplateChainInfo, ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use crate::methods::types::get_block_template::{
    validate_block_proposal, BlockProposalResponse, GetBlockTemplateResponse,
    MAX_CONCURRENT_PROPOSAL_CHECKS, PROPOSAL_CHECK_TIMEOUT,
};

/// NU7 is scheduled after issuance's fourth halving, so reissuance has no crossing.
fn fixture() -> (Network, Block, GetBlockTemplateChainInfo) {
    let network = Parameters::build()
        .with_disable_pow(true)
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(20_000_001),
            ..Network::new_default_testnet().activation_list().into()
        })
        .unwrap()
        .clear_funding_streams()
        .with_lockbox_disbursements(Vec::new())
        .to_network()
        .unwrap();
    assert_eq!(network.nsm_reissuance_height(), None);
    let block: Block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let chain_info = GetBlockTemplateChainInfo {
        tip_height: Height::MIN,
        tip_hash: block.header.previous_block_hash,
        expected_difficulty: block.header.difficulty_threshold,
        cur_time: block.header.time.try_into().unwrap(),
        min_time: block.header.time.try_into().unwrap(),
        max_time: block.header.time.try_into().unwrap(),
        chain_history_root: None,
        chain_value_pools: Default::default(),
    };
    (network, block, chain_info)
}

async fn check(
    verifier: impl BlockVerifierService,
    block: &Block,
    network: &Network,
    chain_info: &GetBlockTemplateChainInfo,
    admission: Arc<Semaphore>,
) -> RpcResult<GetBlockTemplateResponse> {
    let chain_info = chain_info.clone();
    let read_state = tower::service_fn(move |request| {
        assert!(matches!(request, ReadRequest::ChainInfo));
        ready(Ok::<_, BoxError>(ReadResponse::ChainInfo(
            chain_info.clone(),
        )))
    });
    validate_block_proposal(
        verifier,
        read_state,
        block.zcash_serialize_to_vec().unwrap(),
        network,
        NoChainTip,
        MockSyncStatus::default(),
        admission,
    )
    .await
}

/// Dropping RPC waiters cannot admit replacements while their buffered work is still running.
#[tokio::test(start_paused = true)]
async fn rpc_proposal_admission_outlives_timeout_and_client_cancellation() {
    let _init_guard = zebra_test::init();
    let (network, block, chain_info) = fixture();
    for cancel in [false, true] {
        let admission = Arc::new(Semaphore::new(MAX_CONCURRENT_PROPOSAL_CHECKS));
        let mut verifier: MockService<Request, Hash, _, BoxError> =
            MockService::build().for_unit_tests();
        let buffered = Buffer::new(verifier.clone(), 1);
        let mut callers = Vec::new();
        let mut work = Vec::new();
        for _ in 0..MAX_CONCURRENT_PROPOSAL_CHECKS {
            let mut caller = Box::pin(check(
                buffered.clone(),
                &block,
                &network,
                &chain_info,
                admission.clone(),
            ));
            assert!(futures::poll!(&mut caller).is_pending());
            work.push(
                verifier
                    .expect_request_that(|request| matches!(request, Request::CheckProposal(_)))
                    .await,
            );
            callers.push(caller);
        }
        if !cancel {
            tokio::time::advance(PROPOSAL_CHECK_TIMEOUT).await;
            for caller in &mut callers {
                let error = caller
                    .await
                    .expect_err("timeout is not successful verification");
                assert_eq!(error.message(), "block proposal check timed out");
            }
        }
        drop(callers);
        assert_eq!(admission.available_permits(), 0);
        let error = check(
            buffered.clone(),
            &block,
            &network,
            &chain_info,
            admission.clone(),
        )
        .await
        .expect_err("continuing work must retain every admission slot");
        assert!(error.message().contains("too many block proposals"));
        for response in work {
            response.respond(Err::<Hash, BoxError>(
                VerifyBlockError::OversizedBlock.into(),
            ));
        }
        // Wait on the permit itself rather than guessing how many executor turns drain responses.
        let released = tokio::time::timeout(
            Duration::from_secs(1),
            admission
                .clone()
                .acquire_many_owned(MAX_CONCURRENT_PROPOSAL_CHECKS.try_into().unwrap()),
        )
        .await
        .expect("finished verifier responses must release admission")
        .unwrap();
        drop(released);
        let (response, ()) = tokio::join!(
            check(buffered, &block, &network, &chain_info, admission.clone()),
            async {
                verifier
                    .expect_request_that(|request| matches!(request, Request::CheckProposal(_)))
                    .await
                    .respond(block.hash());
            },
        );
        assert_eq!(
            response.unwrap(),
            GetBlockTemplateResponse::ProposalMode(BlockProposalResponse::Valid),
        );
        assert_eq!(
            admission.available_permits(),
            MAX_CONCURRENT_PROPOSAL_CHECKS
        );
    }
}

#[derive(Clone)]
struct NeverReady(Arc<AtomicUsize>);

impl Service<Request> for NeverReady {
    type Response = Hash;
    type Error = BoxError;
    type Future = Ready<Result<Hash, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Poll::Pending
    }

    fn call(&mut self, _: Request) -> Self::Future {
        panic!("an unready verifier must not receive proposal work")
    }
}

/// The one admission deadline covers state lookup plus verifier readiness, not just its response.
#[tokio::test(start_paused = true)]
async fn rpc_proposal_deadline_includes_preflight_and_readiness() {
    let _init_guard = zebra_test::init();
    let (network, block, chain_info) = fixture();
    let polls = Arc::new(AtomicUsize::new(0));
    let read_state = tower::service_fn(move |request| {
        assert!(matches!(request, ReadRequest::ChainInfo));
        let chain_info = chain_info.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(9)).await;
            Ok::<_, BoxError>(ReadResponse::ChainInfo(chain_info))
        }
    });
    let admission = Arc::new(Semaphore::new(MAX_CONCURRENT_PROPOSAL_CHECKS));
    let checking = validate_block_proposal(
        NeverReady(polls.clone()),
        read_state,
        block.zcash_serialize_to_vec().unwrap(),
        &network,
        NoChainTip,
        MockSyncStatus::default(),
        admission.clone(),
    );
    tokio::pin!(checking);
    assert!(futures::poll!(&mut checking).is_pending());
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(futures::poll!(&mut checking).is_pending());
    assert!(polls.load(Ordering::SeqCst) > 0);
    tokio::time::advance(PROPOSAL_CHECK_TIMEOUT - Duration::from_secs(9)).await;
    let Poll::Ready(result) = futures::poll!(&mut checking) else {
        panic!("readiness must expire at the admission deadline")
    };
    assert_eq!(
        result.unwrap_err().message(),
        "block proposal check timed out"
    );
    assert_eq!(
        admission.available_permits(),
        MAX_CONCURRENT_PROPOSAL_CHECKS
    );
}

/// A configured NU7 without reissuance must reject a spoofed pre-NU7 height or stale parent.
#[tokio::test(start_paused = true)]
async fn rpc_proposal_preflights_networks_without_reissuance() {
    let _init_guard = zebra_test::init();
    let (network, block, mut chain_info) = fixture();
    chain_info.tip_height = Height(20_000_001);
    let mut next = block.clone();
    let mut inputs = next.transactions[0].inputs();
    let zebra_chain::transparent::Input::Coinbase { height, .. } = &mut inputs[0] else {
        panic!("fixture starts with a coinbase")
    };
    *height = chain_info.tip_height.next().unwrap();
    next.transactions[0] = Arc::new(
        (*next.transactions[0])
            .clone()
            .with_transparent_inputs(inputs),
    );
    let mut stale_parent = next.clone();
    Arc::make_mut(&mut stale_parent.header).previous_block_hash = Hash([9; 32]);
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let verifier = tower::service_fn(move |_: Request| {
        observed.fetch_add(1, Ordering::SeqCst);
        ready(Err::<Hash, BoxError>(
            VerifyBlockError::OversizedBlock.into(),
        ))
    });
    let admission = Arc::new(Semaphore::new(MAX_CONCURRENT_PROPOSAL_CHECKS));
    for proposal in [&block, &stale_parent] {
        assert!(matches!(
            check(
                verifier.clone(),
                proposal,
                &network,
                &chain_info,
                admission.clone(),
            )
            .await
            .unwrap(),
            GetBlockTemplateResponse::ProposalMode(BlockProposalResponse::Rejected(_)),
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    // A matching context reaches the verifier, whose real consensus error stays a rejection.
    assert!(matches!(
        check(verifier, &next, &network, &chain_info, admission.clone())
            .await
            .unwrap(),
        GetBlockTemplateResponse::ProposalMode(BlockProposalResponse::Rejected(_)),
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let read_state =
        tower::service_fn(|_| ready(Err::<ReadResponse, BoxError>("state is unavailable".into())));
    let error = validate_block_proposal(
        NeverReady(Arc::new(AtomicUsize::new(0))),
        read_state,
        next.zcash_serialize_to_vec().unwrap(),
        &network,
        NoChainTip,
        MockSyncStatus::default(),
        admission,
    )
    .await
    .expect_err("unavailable context is an RPC error, not a consensus rejection");
    assert_eq!(error.message(), "state is unavailable");
}
