//! Proposal admission boundaries, using controlled consensus and real committed state tips.

use std::{collections::HashSet, future::Future, sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::sync::oneshot;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, ZcashDeserializeInto},
    transaction::VerifiedUnminedTx,
    transparent::OutPoint,
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
};
use zebra_consensus::transaction;
use zebra_node_services::mempool::{Gossip, Request, Response};
use zebra_state::{ReadRequest, ReadResponse};
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::{
    super::{
        admission::{
            required_package, validation_miner_params, Admission, Candidate, Verdict,
            COINBASE_RESERVE_BYTES, COINBASE_RESERVE_SIGOPS, MAX_PACKAGE_COUNT,
        },
        storage::{ExactTipRejectionError, SameEffectsChainRejectionError},
        Mempool, MempoolError, Storage,
    },
    vector::{setup, MockTxVerifier},
};
use crate::BoxError;

type ProposalVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;
type ProposalResponse =
    zebra_test::mock_service::ResponseSender<zebra_consensus::Request, block::Hash, BoxError>;

/// These tests control semantic verification; chain info supplies a supported template era while
/// parent hashes and final freshness checks still come from the ephemeral committed state.
pub(super) fn mock_proposals(mempool: &mut Mempool) -> ProposalVerifier {
    mock_proposals_with_width(mempool, super::super::config::DEFAULT_ADMISSION_SPLIT_WIDTH)
}

fn mock_proposals_with_width(mempool: &mut Mempool, split_width: usize) -> ProposalVerifier {
    let verifier = MockService::build().for_unit_tests();
    mempool.admission = Admission::new(
        mempool.network.clone(),
        admission_read_state(mempool.state.clone(), &mempool.network),
        Buffer::new(BoxService::new(verifier.clone()), 1),
        split_width,
    );
    verifier
}

/// Gives the mempool a template scheduler with a miner address and a controlled block verifier.
///
/// The default `setup()` has no miner address, so its scheduler never builds a template.
fn mock_templates(
    mempool: &mut Mempool,
) -> (
    ProposalVerifier,
    tokio::sync::watch::Receiver<Option<Arc<zebra_rpc::BlockTemplateResponse>>>,
    tokio::sync::mpsc::Sender<zebra_rpc::BlockTemplateRequest>,
) {
    // The scheduler can wait out a retry delay before it rebuilds, which is longer than the
    // mock's default request deadline.
    let verifier: ProposalVerifier = MockService::build()
        .with_max_request_delay(Duration::from_secs(10))
        .for_unit_tests();
    let miner_params = zebra_rpc::MinerParams::from(
        zcash_keys::address::Address::decode(
            &mempool.network,
            zebra_rpc::config::mining::default_miner_address(
                mempool.network.kind(),
                &zebra_rpc::config::mining::MinerAddressType::Transparent,
            ),
        )
        .expect("the hard-coded transparent address is valid"),
    );

    let (templates, published, requests) = super::super::block_template::BlockTemplates::new(
        mempool.network.clone(),
        Some(miner_params),
        admission_read_state(mempool.state.clone(), &mempool.network),
        Buffer::new(BoxService::new(verifier.clone()), 1),
    );
    mempool.block_templates = templates;

    (verifier, published, requests)
}

/// Keep historical network fixtures focused on their caller behavior, not block consensus.
pub(crate) fn admission_read_state<State: zebra_state::State>(
    state: State,
    network: &Network,
) -> super::super::block_template::ReadState {
    let height = NetworkUpgrade::Nu5.activation_height(network).unwrap();
    let service =
        tower::service_fn(move |request| read_admission_state(state.clone(), height, request));
    Buffer::new(BoxService::new(service), 1)
}

async fn read_admission_state<State: zebra_state::State>(
    state: State,
    height: block::Height,
    request: ReadRequest,
) -> Result<ReadResponse, BoxError> {
    let zebra_state::Response::Tip(tip) = state.oneshot(zebra_state::Request::Tip).await? else {
        unreachable!("Tip response expected")
    };
    Ok(match request {
        ReadRequest::Tip => ReadResponse::Tip(tip),
        ReadRequest::ChainInfo => ReadResponse::ChainInfo(zebra_state::GetBlockTemplateChainInfo {
            tip_height: height,
            tip_hash: tip.expect("fixture committed genesis").1,
            chain_history_root: Some([0; 32].into()),
            expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
            cur_time: DateTime32::from(1_654_008_617),
            min_time: DateTime32::from(1_654_008_606),
            max_time: DateTime32::from(1_654_008_719),
        }),
        _ => panic!("unexpected admission state request"),
    })
}

/// Drive the actual mempool service, just as QueueChecker does in the node.
pub(super) async fn drive<F: Future>(mempool: &mut Mempool, future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => return result,
                _ = tokio::time::sleep(Duration::from_millis(1)) => mempool.dummy_call().await,
            }
        }
    })
    .await
    .expect("admission fixture must make progress")
}

pub(super) fn candidate() -> VerifiedUnminedTx {
    Network::Mainnet
        .unmined_transactions_in_blocks(982_681..=982_681)
        .find(|tx| {
            !tx.transaction.transaction.is_coinbase()
                && !tx.transaction.transaction.outputs().is_empty()
        })
        .expect("the historical block fixture contains a transparent spend")
}

async fn queue_candidate(
    mempool: &mut Mempool,
    tx: &VerifiedUnminedTx,
) -> Result<oneshot::Receiver<Result<(), BoxError>>, BoxError> {
    let Response::Queued(mut queued) = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![Gossip::Tx(tx.transaction.clone())]))
        .await
        .unwrap()
    else {
        panic!("Queue response expected")
    };
    queued.remove(0)
}

#[tokio::test]
async fn rejected_proposal_does_not_trigger_an_empty_template_fill() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let (mut templates, mut published, _requests) = mock_templates(&mut mempool);
    let mut proposals = mock_proposals(&mut mempool);
    let coinbase = drive(&mut mempool, templates.expect_request_that(|_| true)).await;
    let tx = candidate();
    let result = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx));
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(Err::<block::Hash, BoxError>(
            zebra_consensus::RouterError::from(zebra_consensus::VerifyBlockError::Transaction(
                zebra_consensus::error::TransactionError::BadBalance,
            ))
            .into(),
        ));
    assert!(drive(&mut mempool, result).await.unwrap().is_err());
    coinbase.respond(block::Hash([0; 32]));
    drive(&mut mempool, published.changed()).await.unwrap();
    assert!(published
        .borrow()
        .as_ref()
        .unwrap()
        .transactions()
        .is_empty());
    assert!(
        drive(
            &mut mempool,
            tokio::time::timeout(
                Duration::from_secs(1),
                templates.expect_request_that(|_| true)
            ),
        )
        .await
        .is_err(),
        "a rejected proposal must not request an immediate empty mempool fill"
    );
}

#[tokio::test]
async fn self_eviction_does_not_trigger_an_empty_template_fill() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, 0, true).await;
    mempool.enable(&mut recent_syncs).await;
    let (mut templates, mut published, _requests) = mock_templates(&mut mempool);
    let coinbase = drive(&mut mempool, templates.expect_request_that(|_| true)).await;
    let tx = candidate();
    let result = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx));
    let error = drive(&mut mempool, result).await.unwrap().unwrap_err();
    assert_eq!(
        error.downcast_ref::<MempoolError>(),
        Some(&MempoolError::StorageEffectsChain(
            SameEffectsChainRejectionError::RandomlyEvicted
        )),
    );
    assert!(mempool.storage().transactions().is_empty());
    coinbase.respond(block::Hash([0; 32]));
    drive(&mut mempool, published.changed()).await.unwrap();
    assert!(published
        .borrow()
        .as_ref()
        .unwrap()
        .transactions()
        .is_empty());
    assert!(
        drive(
            &mut mempool,
            tokio::time::timeout(
                Duration::from_secs(1),
                templates.expect_request_that(|_| true)
            ),
        )
        .await
        .is_err(),
        "self-eviction leaves no transaction requiring an initial fill"
    );
}

#[tokio::test]
async fn deterministic_proposal_rejection_is_cached_until_the_tip_changes() {
    use zebra_state::DuplicateNullifierError;

    let (mut mempool, _, mut state, mut tip_change, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    let tx = candidate();
    let result = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx.clone()));
    // Follow the actual CheckProposal -> ValidateProposal -> ValidateContextError path.
    let context = zebra_chain::sapling::Nullifier::from([1; 32]).duplicate_nullifier_error(false);
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(Err::<block::Hash, BoxError>(
            zebra_consensus::RouterError::from(
                zebra_consensus::VerifyBlockError::ValidateProposal(context.into()),
            )
            .into(),
        ));
    let error = drive(&mut mempool, result).await.unwrap().unwrap_err();
    assert!(
        error
            .downcast_ref::<zebra_consensus::RouterError>()
            .is_some(),
        "the first caller keeps the original consensus error"
    );
    let replay = queue_candidate(&mut mempool, &tx).await.unwrap_err();
    assert!(matches!(
        replay.downcast_ref::<MempoolError>(),
        Some(MempoolError::StorageExactTip(
            ExactTipRejectionError::FailedProposal { .. }
        )),
    ));

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block.into(),
        ))
        .await
        .unwrap();
    tip_change.wait_for_tip_change().await.unwrap();
    mempool.dummy_call().await;
    let retry = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx.clone()));
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(block::Hash([0; 32]));
    drive(&mut mempool, retry).await.unwrap().unwrap();
    assert!(mempool
        .storage()
        .contains_transaction_exact(&tx.transaction.id.mined_id()));
}

#[tokio::test]
async fn proposal_service_failures_are_retryable() {
    use zebra_consensus::{error::TransactionError, RouterError, VerifyBlockError};

    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    let tx = candidate();
    let errors: Vec<BoxError> = vec![
        "transient verifier failure".into(),
        RouterError::from(VerifyBlockError::ValidateProposal("state not ready".into())).into(),
        RouterError::from(VerifyBlockError::ValidateProposal(
            zebra_state::CommitSemanticallyVerifiedError::from(
                zebra_state::CommitBlockError::Duplicate {
                    hash_or_height: None,
                    location: zebra_state::KnownBlock::BestChain,
                },
            )
            .into(),
        ))
        .into(),
        RouterError::from(VerifyBlockError::Transaction(
            TransactionError::InternalDowncastError("lost transaction service".into()),
        ))
        .into(),
    ];
    for error in errors {
        let result = queue_candidate(&mut mempool, &tx).await.unwrap();
        drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
            .await
            .respond(transaction::MempoolResponse::from(tx.clone()));
        drive(&mut mempool, proposals.expect_request_that(|_| true))
            .await
            .respond(Err::<block::Hash, BoxError>(error));
        assert!(drive(&mut mempool, result).await.unwrap().is_err());
    }
    let result = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx.clone()));
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(block::Hash([0; 32]));
    drive(&mut mempool, result).await.unwrap().unwrap();
    assert!(mempool
        .storage()
        .contains_transaction_exact(&tx.transaction.id.mined_id()));
}

#[tokio::test]
async fn timed_out_proposal_drains_without_caching_its_late_rejection() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    let tx = candidate();
    let mut result = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx.clone()));
    let held = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
    tokio::time::pause();
    tokio::time::advance(
        super::super::downloads::TRANSACTION_VERIFY_TIMEOUT + Duration::from_secs(1),
    )
    .await;
    mempool.dummy_call().await;
    assert!(matches!(
        result.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(mempool.tx_downloads().in_flight(), 1);
    held.respond(Err::<block::Hash, BoxError>(
        zebra_consensus::RouterError::from(zebra_consensus::VerifyBlockError::Transaction(
            zebra_consensus::error::TransactionError::BadBalance,
        ))
        .into(),
    ));
    tokio::time::resume();
    let error = drive(&mut mempool, result).await.unwrap().unwrap_err();
    assert!(error
        .downcast_ref::<tokio::time::error::Elapsed>()
        .is_some());
    let retry = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx.clone()));
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(block::Hash([0; 32]));
    drive(&mut mempool, retry).await.unwrap().unwrap();
    assert!(mempool
        .storage()
        .contains_transaction_exact(&tx.transaction.id.mined_id()));
}

#[tokio::test]
async fn oversized_package_rejection_is_cached() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let tx = candidate();
    let mut oversized = tx.clone();
    oversized.transaction.size = usize::try_from(block::MAX_BLOCK_BYTES).unwrap() + 1;
    let result = queue_candidate(&mut mempool, &tx).await.unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(oversized));
    assert!(drive(&mut mempool, result).await.unwrap().is_err());
    assert!(matches!(
        queue_candidate(&mut mempool, &tx)
            .await
            .unwrap_err()
            .downcast_ref::<MempoolError>(),
        Some(MempoolError::StorageExactTip(
            ExactTipRejectionError::FailedProposal { .. }
        )),
    ));
}

#[tokio::test]
async fn stale_package_limit_failure_is_not_cached() {
    let (mut mempool, _, mut state, mut tip_change, _tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let _proposals = mock_proposals(&mut mempool);
    let genesis: Block = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let old_parent = genesis.hash();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block.into(),
        ))
        .await
        .unwrap();
    tip_change.wait_for_tip_change().await.unwrap();

    // Model a committed state tip advancing before its notification is reconciled by the mempool.
    let mut storage = Storage::new(&super::super::Config::default());
    let mut tx = candidate();
    let id = tx.transaction.id;
    tx.transaction.size = usize::try_from(block::MAX_BLOCK_BYTES).unwrap() + 1;
    mempool
        .admission
        .push(Candidate::new(tx, Vec::new(), Some(old_parent), None));
    let outcomes = tokio::time::timeout(
        Duration::from_secs(10),
        futures::future::poll_fn(|cx| mempool.admission.poll(cx, &mut storage, old_parent)),
    )
    .await
    .unwrap();
    assert!(matches!(outcomes.as_slice(), [(_, Verdict::Retry)]));
    assert!(storage.should_download_or_verify(id).is_ok());
}

#[tokio::test]
async fn rejected_proposal_never_releases_outputs_or_gossip() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, mut changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    let tx = candidate();
    let id = tx.transaction.id;
    let outpoint = OutPoint::from_usize(id.mined_id(), 0);
    let mut output = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::AwaitOutput(outpoint));
    let Response::Queued(mut queued) = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![Gossip::Tx(tx.transaction.clone())]))
        .await
        .unwrap()
    else {
        panic!("Queue response expected")
    };
    let mut result = queued.remove(0).unwrap();
    tx_verifier
        .expect_request_that(|_| true)
        .await
        .respond(transaction::MempoolResponse::from(tx));
    let proposal = drive(
        &mut mempool,
        proposals.expect_request_that(|request| {
            matches!(request, zebra_consensus::Request::CheckProposal(_))
        }),
    )
    .await;
    assert!(matches!(
        result.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(output.as_mut().now_or_never().is_none());
    assert!(changes.try_recv().is_err());
    let Response::TransactionIds(ids) = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::TransactionIds)
        .await
        .unwrap()
    else {
        panic!("ids response expected")
    };
    assert!(!ids.contains(&id));
    proposal.respond(Err::<block::Hash, BoxError>(
        "aggregate proposal failure".into(),
    ));
    assert!(drive(&mut mempool, result).await.unwrap().is_err());
    assert!(mempool.storage().created_output(&outpoint).is_none());
    assert!(output.as_mut().now_or_never().is_none());
    while let Ok(change) = changes.try_recv() {
        assert_ne!(
            change,
            zebra_node_services::mempool::MempoolChange::added([id].into_iter().collect())
        );
    }
}

/// Checks that a transaction which fails verification doesn't make Zebra rebuild and revalidate
/// the block template.
///
/// Such a transaction never entered the verified set, so the template still describes it
/// correctly. Rebuilding anyway re-runs selection, constructs a candidate block, and submits the
/// whole block to `CheckProposal`, revalidating every script and shielded proof in it. A peer can
/// pace invalid transactions to keep Zebra doing that: re-signing a v5 transaction gives each
/// attempt a fresh `UnminedTxId` with the same effects, so exact rejection caching doesn't stop
/// the stream.
#[tokio::test]
async fn a_failed_verification_does_not_rebuild_the_template() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let (mut proposals, mut published, _requests) = mock_templates(&mut mempool);

    // Hold the first coinbase build: only a real mutation should request an immediate fill.
    let first_build = drive(
        &mut mempool,
        proposals.expect_request_that(|request| {
            matches!(request, zebra_consensus::Request::CheckProposal(_))
        }),
    )
    .await;

    let tx = candidate();
    let Response::Queued(mut queued) = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![Gossip::Tx(tx.transaction.clone())]))
        .await
        .unwrap()
    else {
        panic!("Queue response expected")
    };
    let mut result = queued.remove(0).unwrap();

    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond_error(zebra_consensus::error::TransactionError::WrongVersion);

    // The queued transaction's caller learns it failed.
    assert!(drive(&mut mempool, &mut result).await.unwrap().is_err());
    first_build.respond(block::Hash([0; 32]));
    drive(&mut mempool, published.changed()).await.unwrap();

    // The verified set never changed, so there must be no immediate empty fill before the
    // bounded same-tip refresh. Keep polling the service while observing this window.
    let rebuilt = drive(
        &mut mempool,
        tokio::time::timeout(
            Duration::from_secs(1),
            proposals.expect_request_that(|request| {
                matches!(request, zebra_consensus::Request::CheckProposal(_))
            }),
        ),
    )
    .await;

    assert!(
        rebuilt.is_err(),
        "a transaction that never entered the mempool must not rebuild and revalidate the template"
    );
}

/// Checks that an insertion which evicts transactions and then reports an error still rebuilds
/// the block template.
///
/// `Storage::insert()` evicts transactions to stay under the cost limit, and returns
/// `RandomlyEvicted` when the incoming transaction or one of its ancestors was among them — after
/// it has already removed the others. So an error doesn't mean the verified set is unchanged, and
/// keying the rebuild on a successful insertion leaves the template describing transactions the
/// mempool no longer holds.
#[tokio::test]
async fn an_evicting_insertion_rebuilds_the_template() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let stored = candidate();
    let stored_id = stored.transaction.id;
    mempool.storage().insert(stored, Vec::new(), None).unwrap();
    let (mut proposals, mut published, _requests) = mock_templates(&mut mempool);

    // New-tip coinbase work is followed by a fill containing the real stored transaction.
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(block::Hash([0; 32]));
    drive(&mut mempool, published.changed()).await.unwrap();
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(block::Hash([0; 32]));
    drive(&mut mempool, published.changed()).await.unwrap();
    let old_template = published.borrow_and_update().clone().unwrap();
    let old_block =
        zebra_rpc::proposal_block_from_template(&old_template, None, &Network::Mainnet).unwrap();
    assert_eq!(old_block.transactions[1].unmined_id(), stored_id);

    // Lower the fixture's cost budget only after publication, making eviction deterministic
    // regardless of ZIP-401's random victim order. This is real-to-empty, not empty-to-empty.
    super::super::storage::tests::set_tx_cost_limit(mempool.storage(), 0);
    let tx = Network::Mainnet
        .unmined_transactions_in_blocks(982_681..=982_681)
        .find(|tx| !tx.transaction.transaction.is_coinbase() && tx.transaction.id != stored_id)
        .expect("the block contains another non-conflicting transaction");
    let Response::Queued(mut queued) = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![Gossip::Tx(tx.transaction.clone())]))
        .await
        .unwrap()
    else {
        panic!("Queue response expected")
    };
    let result = queued.remove(0).unwrap();
    drive(&mut mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse::from(tx));
    let error = drive(&mut mempool, result).await.unwrap().unwrap_err();
    assert_eq!(
        error.downcast_ref::<MempoolError>(),
        Some(&MempoolError::StorageEffectsChain(
            SameEffectsChainRejectionError::RandomlyEvicted
        )),
    );
    assert!(mempool.storage().transactions().is_empty());

    // Same-tip changes are coalesced until the bounded refresh; inspect the replacement,
    // rather than merely observing that some proposal was checked.
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(block::Hash([0; 32]));
    drive(&mut mempool, published.changed()).await.unwrap();
    assert!(published
        .borrow()
        .as_ref()
        .unwrap()
        .transactions()
        .is_empty());
    assert_eq!(old_template.transactions().len(), 1);
}

#[tokio::test]
async fn stale_proposal_reverifies_without_releasing_its_response() {
    // Exercise Grow, Reset, and a committed tip that advances before its notification.
    for (block_count, lagging_notification) in [(1, false), (2, false), (1, true)] {
        let (
            mut mempool,
            _,
            mut state,
            mut tip_change,
            mut tx_verifier,
            mut recent_syncs,
            mut changes,
        ) = setup(&Network::Mainnet, u64::MAX, true).await;
        mempool.enable(&mut recent_syncs).await;
        let mut proposals = mock_proposals(&mut mempool);
        let tx = candidate();
        let id = tx.transaction.id;
        let Response::Queued(mut queued) = mempool
            .ready()
            .await
            .unwrap()
            .call(Request::Queue(vec![Gossip::Tx(tx.transaction.clone())]))
            .await
            .unwrap()
        else {
            panic!("Queue response expected")
        };
        let mut result = queued.remove(0).unwrap();
        tx_verifier
            .expect_request_that(|_| true)
            .await
            .respond(transaction::MempoolResponse::from(tx));
        let held = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
        let _stale_tip_sender = if lagging_notification {
            let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
                .zcash_deserialize_into()
                .unwrap();
            let tip = zebra_state::ChainTipBlock::from(zebra_state::CheckpointVerifiedBlock::from(
                genesis,
            ));
            let (sender, latest, mut changes) =
                zebra_state::ChainTipSender::new(Some(tip), &Network::Mainnet);
            let _initial_notification = changes.last_tip_change();
            mempool.latest_chain_tip = latest;
            mempool.chain_tip_change = changes;
            Some(sender)
        } else {
            None
        };
        let blocks = [
            zebra_test::vectors::BLOCK_MAINNET_1_BYTES.as_slice(),
            zebra_test::vectors::BLOCK_MAINNET_2_BYTES.as_slice(),
        ];
        for bytes in blocks.into_iter().take(block_count) {
            let block: Arc<Block> = bytes.zcash_deserialize_into().unwrap();
            state
                .ready()
                .await
                .unwrap()
                .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
                    block.into(),
                ))
                .await
                .unwrap();
            tip_change.wait_for_tip_change().await.unwrap();
        }
        mempool.dummy_call().await;
        assert_eq!(mempool.tx_downloads().in_flight(), 1);
        assert!(matches!(
            result.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(mempool
            .storage()
            .transactions()
            .get(&id.mined_id())
            .is_none());
        // Even a failure for the old parent is stale work, not a rejection of the transaction.
        held.respond(Err::<block::Hash, BoxError>(
            zebra_consensus::RouterError::from(zebra_consensus::VerifyBlockError::Transaction(
                zebra_consensus::error::TransactionError::BadBalance,
            ))
            .into(),
        ));
        let retry = drive(&mut mempool, tx_verifier.expect_request_that(|_| true)).await;
        assert_eq!(retry.request().transaction.id, id);
        assert!(matches!(
            result.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(changes.try_recv().is_err());
        retry.respond(Err(zebra_consensus::error::TransactionError::BadBalance));
    }
}

/// Queue a transaction and answer its semantic verification with `tx` itself.
async fn queue_verified(
    mempool: &mut Mempool,
    tx_verifier: &mut MockTxVerifier,
    tx: &VerifiedUnminedTx,
    spent_mempool_outpoints: Vec<OutPoint>,
) -> oneshot::Receiver<Result<(), BoxError>> {
    let result = queue_candidate(mempool, tx).await.unwrap();
    drive(mempool, tx_verifier.expect_request_that(|_| true))
        .await
        .respond(transaction::MempoolResponse {
            transaction: tx.clone(),
            spent_mempool_outpoints,
        });
    result
}

/// Admit `txs[0]` alone, while `txs[1..]` are verified and wait to share the next proposal.
async fn hold_then_queue(
    mempool: &mut Mempool,
    tx_verifier: &mut MockTxVerifier,
    proposals: &mut ProposalVerifier,
    txs: &[VerifiedUnminedTx],
) -> Vec<oneshot::Receiver<Result<(), BoxError>>> {
    let first = queue_verified(mempool, tx_verifier, &txs[0], Vec::new()).await;
    let held = drive(mempool, proposals.expect_request_that(|_| true)).await;
    assert_eq!(proposed(held.request()), [txs[0].transaction.id]);
    let mut results = Vec::new();
    for tx in &txs[1..] {
        results.push(queue_verified(mempool, tx_verifier, tx, Vec::new()).await);
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while mempool.admission.queued() < txs.len() - 1 {
            mempool.dummy_call().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("verified candidates reach admission");
    held.respond(block::Hash([0; 32]));
    drive(mempool, first).await.unwrap().unwrap();
    results
}

/// The non-coinbase transactions in a proposal, in block order.
fn proposed(request: &zebra_consensus::Request) -> Vec<zebra_chain::transaction::UnminedTxId> {
    let zebra_consensus::Request::CheckProposal(block) = request else {
        panic!("admission only checks proposals")
    };
    block.transactions[1..]
        .iter()
        .map(|tx| tx.unmined_id())
        .collect()
}

fn block_transactions(count: usize) -> Vec<VerifiedUnminedTx> {
    let txs: Vec<_> = Network::Mainnet
        .unmined_transactions_in_blocks(982_681..)
        .filter(|tx| {
            !tx.transaction.transaction.is_coinbase()
                && !tx.transaction.transaction.inputs().is_empty()
        })
        .take(count)
        .collect();
    assert_eq!(
        txs.len(),
        count,
        "the block fixtures have enough transparent spends"
    );
    txs
}

fn proposal_error(
    error: zebra_consensus::error::TransactionError,
) -> Result<block::Hash, BoxError> {
    Err(
        zebra_consensus::RouterError::from(zebra_consensus::VerifyBlockError::Transaction(error))
            .into(),
    )
}

/// Only a candidate that failed a proposal alone is cached as a rejection.
async fn assert_cached_rejection(mempool: &mut Mempool, tx: &VerifiedUnminedTx) {
    assert!(matches!(
        queue_candidate(mempool, tx)
            .await
            .unwrap_err()
            .downcast_ref::<MempoolError>(),
        Some(MempoolError::StorageExactTip(
            ExactTipRejectionError::FailedProposal { .. }
        )),
    ));
}

#[tokio::test]
async fn failed_batch_is_split_into_concurrent_pieces_until_attributed() {
    use super::super::config::DEFAULT_ADMISSION_SPLIT_WIDTH as WIDTH;
    use zebra_consensus::error::TransactionError::BadBalance;

    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    // One held candidate, then two candidates for each piece of the failed batch.
    let txs = block_transactions(1 + 2 * WIDTH);
    let ids: Vec<_> = txs.iter().map(|tx| tx.transaction.id).collect();
    let invalid = 6;

    let results = hold_then_queue(&mut mempool, &mut tx_verifier, &mut proposals, &txs).await;
    let batch = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
    assert_eq!(proposed(batch.request()), ids[1..]);
    batch.respond(proposal_error(BadBalance));

    // All pieces are in flight at once, before any of them is answered.
    let mut pieces = Vec::new();
    for _ in 0..WIDTH {
        pieces.push(drive(&mut mempool, proposals.expect_request_that(|_| true)).await);
    }
    let proposed_pieces: HashSet<_> = pieces
        .iter()
        .map(|piece| proposed(piece.request()))
        .collect();
    let expected: HashSet<_> = ids[1..].chunks(2).map(<[_]>::to_vec).collect();
    assert_eq!(proposed_pieces, expected);

    let answer = |pieces: Vec<ProposalResponse>| {
        for piece in pieces {
            if proposed(piece.request()).contains(&ids[invalid]) {
                piece.respond(proposal_error(BadBalance));
            } else {
                piece.respond(block::Hash([0; 32]));
            }
        }
    };
    answer(pieces);

    // The failed piece is split again, into single candidates.
    let mut singles = Vec::new();
    for _ in 0..2 {
        let single = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
        assert_eq!(proposed(single.request()).len(), 1);
        singles.push(single);
    }
    answer(singles);

    for (id, result) in ids[1..].iter().zip(results) {
        let result = drive(&mut mempool, result).await.unwrap();
        assert_eq!(result.is_err(), *id == ids[invalid]);
        assert_eq!(
            mempool.storage().contains_transaction_exact(&id.mined_id()),
            *id != ids[invalid]
        );
    }
    assert_cached_rejection(&mut mempool, &txs[invalid]).await;
}

#[tokio::test]
async fn split_width_is_configurable() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals_with_width(&mut mempool, 2);
    let txs = block_transactions(5);
    let ids: Vec<_> = txs.iter().map(|tx| tx.transaction.id).collect();

    let results = hold_then_queue(&mut mempool, &mut tx_verifier, &mut proposals, &txs).await;
    drive(&mut mempool, proposals.expect_request_that(|_| true))
        .await
        .respond(proposal_error(
            zebra_consensus::error::TransactionError::BadBalance,
        ));

    // A width of 2 splits four candidates into halves, not single candidates.
    let mut halves = HashSet::new();
    for _ in 0..2 {
        let half = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
        halves.insert(proposed(half.request()));
        half.respond(block::Hash([0; 32]));
    }
    assert_eq!(halves, ids[1..].chunks(2).map(<[_]>::to_vec).collect());
    for result in results {
        drive(&mut mempool, result).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn batches_leave_room_for_the_header_and_coinbase() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    let mut txs = block_transactions(3);
    // Together they fit the raw block size limit, but not with a header and coinbase.
    let half = usize::try_from(block::MAX_BLOCK_BYTES).unwrap() / 2;
    txs[1].transaction.size = half;
    txs[2].transaction.size = half - 100;

    let results = hold_then_queue(&mut mempool, &mut tx_verifier, &mut proposals, &txs).await;
    for tx in &txs[1..] {
        let proposal = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
        assert_eq!(proposed(proposal.request()), [tx.transaction.id]);
        proposal.respond(block::Hash([0; 32]));
    }
    for result in results {
        drive(&mut mempool, result).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn conflicting_candidates_are_not_batched_together() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    let mut txs = block_transactions(3);
    txs[2].transaction = Arc::new(
        (*txs[2].transaction.transaction)
            .clone()
            .with_transparent_inputs(txs[1].transaction.transaction.inputs().to_vec()),
    )
    .into();

    let mut results = hold_then_queue(&mut mempool, &mut tx_verifier, &mut proposals, &txs).await;
    let conflict = results.pop().unwrap();
    let spender = results.pop().unwrap();

    // The later conflicting candidate is deferred, rather than failing the earlier one's batch.
    let batch = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
    assert_eq!(proposed(batch.request()), [txs[1].transaction.id]);
    batch.respond(block::Hash([0; 32]));
    drive(&mut mempool, spender).await.unwrap().unwrap();

    // Alone, it forms a valid proposal, but storage rejects the double-spend.
    let alone = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
    assert_eq!(proposed(alone.request()), [txs[2].transaction.id]);
    alone.respond(block::Hash([0; 32]));
    assert!(drive(&mut mempool, conflict).await.unwrap().is_err());
}

/// A child that double-spends an input of its mempool parent passes semantic verification,
/// which only sees the chain UTXO set, so its proposal must still reject it.
#[tokio::test]
async fn candidate_conflicting_with_its_ancestor_is_rejected() {
    let (mut mempool, _, _, _, mut tx_verifier, mut recent_syncs, _changes) =
        setup(&Network::Mainnet, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;
    let mut proposals = mock_proposals(&mut mempool);
    let txs = block_transactions(2);
    let parent = txs[0].clone();
    assert!(!parent.transaction.transaction.outputs().is_empty());
    let parent_output = OutPoint::from_usize(parent.transaction.id.mined_id(), 0);
    mempool
        .storage()
        .insert(parent.clone(), Vec::new(), None)
        .unwrap();

    let mut inputs = parent.transaction.transaction.inputs().to_vec();
    let mut spend_parent = inputs[0].clone();
    let zebra_chain::transparent::Input::PrevOut { outpoint, .. } = &mut spend_parent else {
        panic!("the fixture spends a transparent output")
    };
    *outpoint = parent_output;
    inputs.push(spend_parent);
    let mut child = txs[1].clone();
    child.transaction = Arc::new(
        (*child.transaction.transaction)
            .clone()
            .with_transparent_inputs(inputs),
    )
    .into();

    let result = queue_verified(&mut mempool, &mut tx_verifier, &child, vec![parent_output]).await;
    let proposal = drive(&mut mempool, proposals.expect_request_that(|_| true)).await;
    assert_eq!(
        proposed(proposal.request()),
        [parent.transaction.id, child.transaction.id]
    );
    proposal.respond(proposal_error(
        zebra_consensus::error::TransactionError::DuplicateTransparentSpend(
            parent.transaction.transaction.inputs()[0]
                .outpoint()
                .expect("the fixture spends a transparent output"),
        ),
    ));
    assert!(drive(&mut mempool, result).await.unwrap().is_err());
    assert_cached_rejection(&mut mempool, &child).await;
}

/// Batches reserve room for the admission coinbase, so a full batch does not fail its proposal
/// only because the coinbase pushes the block over its limits.
#[test]
fn coinbase_reserve_covers_the_admission_coinbase() {
    use zebra_chain::{amount::Amount, parameters::NetworkUpgrade::*};

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        let miner_params = validation_miner_params(&network);
        for upgrade in [Canopy, Nu5, Nu6, Nu6_1] {
            let Some(height) = upgrade.activation_height(&network) else {
                continue;
            };
            let coinbase = zebra_rpc::TransactionTemplate::new_coinbase(
                &network,
                height,
                &miner_params,
                Amount::zero(),
            )
            .expect("the validation coinbase is valid");
            assert!(coinbase.data().as_ref().len() <= COINBASE_RESERVE_BYTES);
            assert!(coinbase.sigops() <= COINBASE_RESERVE_SIGOPS);
        }
    }
}

#[test]
fn required_package_is_complete_topological_and_bounded() {
    let mut storage = Storage::new(&super::super::Config {
        tx_cost_limit: u64::MAX,
        ..Default::default()
    });
    let transactions: Vec<_> = Network::Mainnet
        .unmined_transactions_in_blocks(1..=10)
        .filter(|tx| !tx.transaction.transaction.outputs().is_empty())
        .take(4)
        .collect();
    assert_eq!(transactions.len(), 4);
    let mut spent = Vec::new();
    for tx in &transactions[..3] {
        storage.insert(tx.clone(), spent, None).unwrap();
        spent = vec![OutPoint::from_usize(tx.transaction.id.mined_id(), 0)];
    }
    // Repeated direct dependencies must not duplicate shared ancestors.
    spent.push(spent[0]);
    let package = required_package(&storage, &transactions[3], &spent, &mut Vec::new()).unwrap();
    assert_eq!(
        package
            .iter()
            .map(|tx| tx.transaction.id)
            .collect::<Vec<_>>(),
        transactions
            .iter()
            .map(|tx| tx.transaction.id)
            .collect::<Vec<_>>()
    );

    let mut oversized = transactions[3].clone();
    oversized.transaction.size = usize::try_from(block::MAX_BLOCK_BYTES).unwrap();
    assert!(required_package(&storage, &oversized, &spent, &mut Vec::new()).is_err());
    let mut too_many_sigops = transactions[3].clone();
    too_many_sigops.legacy_sigop_count = zebra_consensus::MAX_BLOCK_SIGOPS + 1;
    assert!(required_package(&storage, &too_many_sigops, &spent, &mut Vec::new()).is_err());

    storage.remove_exact(&[transactions[0].transaction.id].into_iter().collect());
    assert!(required_package(&storage, &transactions[3], &spent, &mut Vec::new()).is_err());
}

#[test]
fn required_package_count_limit_counts_unique_ancestors() {
    let mut storage = Storage::new(&super::super::Config {
        tx_cost_limit: u64::MAX,
        ..Default::default()
    });
    // A transparent-only chain with actual parent outpoints; proof verification is not mocked
    // by required_package, which only assembles the closure for the subsequent CheckProposal.
    let base = Network::Mainnet
        .unmined_transactions_in_blocks(1..=1)
        .next()
        .unwrap();
    let input = candidate().transaction.transaction.inputs()[0].clone();
    let mut outpoint = OutPoint::from_usize(zebra_chain::transaction::Hash([0; 32]), 0);
    let mut transactions = Vec::new();
    for _ in 0..=MAX_PACKAGE_COUNT {
        let mut input = input.clone();
        let zebra_chain::transparent::Input::PrevOut {
            outpoint: spent, ..
        } = &mut input
        else {
            panic!("candidate input must be a transparent spend")
        };
        *spent = outpoint;
        let mut tx = base.clone();
        tx.transaction = Arc::new(
            (*base.transaction.transaction)
                .clone()
                .with_transparent_inputs(vec![input]),
        )
        .into();
        outpoint = OutPoint::from_usize(tx.transaction.id.mined_id(), 0);
        transactions.push(tx);
    }
    let mut spent = Vec::new();
    for tx in &transactions[..MAX_PACKAGE_COUNT - 1] {
        storage.insert(tx.clone(), spent, None).unwrap();
        spent = vec![OutPoint::from_usize(tx.transaction.id.mined_id(), 0)];
    }
    // Redundant direct references and a shared transitive ancestor still count only once.
    spent.push(spent[0]);
    spent.push(OutPoint::from_usize(
        transactions[0].transaction.id.mined_id(),
        0,
    ));
    let package = required_package(
        &storage,
        &transactions[MAX_PACKAGE_COUNT - 1],
        &spent,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(
        package
            .iter()
            .map(|tx| tx.transaction.id)
            .collect::<Vec<_>>(),
        transactions[..MAX_PACKAGE_COUNT]
            .iter()
            .map(|tx| tx.transaction.id)
            .collect::<Vec<_>>(),
    );
    storage
        .insert(transactions[MAX_PACKAGE_COUNT - 1].clone(), spent, None)
        .unwrap();
    let spent = [OutPoint::from_usize(
        transactions[MAX_PACKAGE_COUNT - 1]
            .transaction
            .id
            .mined_id(),
        0,
    )];
    assert!(required_package(
        &storage,
        &transactions[MAX_PACKAGE_COUNT],
        &spent,
        &mut Vec::new(),
    )
    .is_err());
}

#[test]
fn proposal_rejection_expires_with_its_ancestor_context() {
    let mut storage = Storage::new(&super::super::Config {
        tx_cost_limit: u64::MAX,
        ..Default::default()
    });
    let ancestor = candidate();
    let id = ancestor.transaction.id;
    storage.insert(ancestor, Vec::new(), None).unwrap();
    let rejected = Network::Mainnet
        .unmined_transactions_in_blocks(1..=1)
        .next()
        .unwrap()
        .transaction
        .id;
    storage.reject(
        rejected,
        ExactTipRejectionError::FailedProposal {
            reason: "invalid ancestor package".into(),
            ancestors: vec![id].into(),
        }
        .into(),
    );
    assert!(storage.should_download_or_verify(rejected).is_err());
    storage.remove_exact(&[id].into_iter().collect());
    assert!(storage.should_download_or_verify(rejected).is_ok());
}
