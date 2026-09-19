//! Proposal admission boundaries, using controlled consensus and real committed state tips.

use std::{future::Future, sync::Arc, time::Duration};

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
        admission::{required_package, Admission},
        Mempool, Storage,
    },
    vector::setup,
};
use crate::BoxError;

type ProposalVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;

/// These tests control semantic verification; chain info supplies a supported template era while
/// parent hashes and final freshness checks still come from the ephemeral committed state.
pub(super) fn mock_proposals(mempool: &mut Mempool) -> ProposalVerifier {
    let verifier = MockService::build().for_unit_tests();
    mempool.admission = Admission::new(
        mempool.network.clone(),
        admission_read_state(mempool.state.clone(), &mempool.network),
        Buffer::new(BoxService::new(verifier.clone()), 1),
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
    let verifier: ProposalVerifier = MockService::build().for_unit_tests();
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
    let (mut proposals, _published, _requests) = mock_templates(&mut mempool);

    // The scheduler builds and validates a template for the current tip. Answering it leaves the
    // template clean, so any later validation can only come from a rebuild.
    let first_build = drive(
        &mut mempool,
        proposals.expect_request_that(|request| {
            matches!(request, zebra_consensus::Request::CheckProposal(_))
        }),
    )
    .await;
    first_build.respond(block::Hash([0; 32]));

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

    // The verified set never changed, so nothing should be rebuilt or revalidated. The mempool
    // has to keep being polled while we check, or the scheduler never runs at all.
    drive(&mut mempool, proposals.expect_no_requests()).await;
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
            "old parent is no longer valid".into(),
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
    let package = required_package(&storage, &transactions[3], &spent).unwrap();
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
    assert!(required_package(&storage, &oversized, &spent).is_err());
    let mut too_many_sigops = transactions[3].clone();
    too_many_sigops.legacy_sigop_count = zebra_consensus::MAX_BLOCK_SIGOPS + 1;
    assert!(required_package(&storage, &too_many_sigops, &spent).is_err());

    storage.remove_exact(&[transactions[0].transaction.id].into_iter().collect());
    assert!(required_package(&storage, &transactions[3], &spent).is_err());
}
