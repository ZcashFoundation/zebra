//! Exact proposal keys and bounded speculative parent lifetimes.

use super::*;
use tower::ServiceExt;
use zebra_chain::{
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserialize,
    transaction::{self, LockTime, Transaction},
    transparent,
    work::difficulty::ParameterDifficulty,
};

fn fixture(height: u32) -> Arc<block::Block> {
    Arc::new(block::Block::zcash_deserialize(zebra_test::vectors::MAINNET_BLOCKS[&height]).unwrap())
}

async fn writer_state() -> (
    tower::buffer::Buffer<super::super::StateService, crate::Request>,
    super::super::ReadStateService,
) {
    use zebra_chain::parameters::testnet::{
        ConfiguredActivationHeights, ConfiguredCheckpoints, Parameters,
    };
    let network = Parameters::build()
        .with_genesis_hash(fixture(0).hash())
        .unwrap()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(2),
            ..Default::default()
        })
        .unwrap()
        // Early Blossom retains the fixture's slow start but needs a shorter capped schedule.
        .with_halving_interval(800_000)
        .unwrap()
        .with_target_difficulty_limit(
            zebra_chain::parameters::Network::Mainnet.target_difficulty_limit(),
        )
        .unwrap()
        .with_funding_streams(Vec::new())
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (block::Height(0), fixture(0).hash()),
            (block::Height(1), fixture(1).hash()),
        ]))
        .unwrap()
        .to_network()
        .unwrap();
    let (state, read, _, _) =
        super::super::StateService::new(crate::Config::ephemeral(), &network, block::Height(1), 0)
            .await;
    let state = tower::buffer::Buffer::new(state, 1);
    for height in 0..=1 {
        state
            .clone()
            .oneshot(crate::Request::CommitCheckpointVerifiedBlock(
                fixture(height).into(),
            ))
            .await
            .unwrap();
    }
    (state, read)
}

#[test]
fn proposal_keys_cover_authorizing_bytes_and_body_count() {
    let _init_guard = zebra_test::init();
    let mut block =
        block::Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..]).unwrap();
    let input = |script| transparent::Input::PrevOut {
        outpoint: transparent::OutPoint {
            hash: transaction::Hash([1; 32]),
            index: 0,
        },
        unlock_script: transparent::Script::new(&[script]),
        sequence: u32::MAX,
    };
    let first = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input(1)],
        vec![],
        LockTime::unlocked(),
        block::Height(1),
    );
    let second = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input(2)],
        vec![],
        LockTime::unlocked(),
        block::Height(1),
    );
    assert_eq!(
        first.hash(),
        second.hash(),
        "ZIP-244 tx IDs exclude transparent authorizing scripts"
    );
    block.transactions = vec![Arc::new(first)];
    let key = proposal_key(&block);
    let serialized = proposal_bytes(&block);
    let mut changed = block.clone();
    changed.transactions = vec![Arc::new(second)];
    assert_ne!(
        key,
        proposal_key(&changed),
        "authorizing bytes must be included, not just transaction IDs"
    );
    assert!(
        !matches_proposal(&changed, &serialized),
        "a reused work ID must still compare authorizing bytes"
    );
    changed = block.clone();
    changed.transactions.push(changed.transactions[0].clone());
    assert_ne!(
        key,
        proposal_key(&changed),
        "the serialized vector count must disambiguate duplicate bodies"
    );
    let header = Arc::make_mut(&mut block.header);
    header.nonce = [42; 32].into();
    header.solution = equihash::Solution::Common([42; 1344]);
    assert_eq!(
        key,
        proposal_key(&block),
        "only nonce and solution can differ"
    );
    assert!(matches_proposal(&block, &serialized));
}

#[tokio::test(start_paused = true)]
async fn dropping_or_expiring_stage_wakes_mining_fallback() {
    let parent = (block::Height(1), block::Hash([1; 32]));
    let child = (block::Height(2), block::Hash([2; 32]));
    for expire in [false, true] {
        let expires = Instant::now() + SPECULATIVE_LIFETIME;
        let (state, _) = watch::channel(MiningState {
            validated_tip: Some(parent),
            staged: Some(Arc::new(Staged {
                parent: parent.1,
                expires,
                state: NonFinalizedState::new(&Network::Mainnet),
            })),
            ..Default::default()
        });
        let (tip, mut updates) = watch::channel(Some(child));
        let guard = guard(state.clone(), tip, expires);
        if expire {
            tokio::time::advance(SPECULATIVE_LIFETIME).await;
        } else {
            drop(guard);
        }
        updates.changed().await.unwrap();
        assert_eq!(*updates.borrow_and_update(), Some(parent));
        assert!(state.borrow().staged.is_none());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn writer_reuses_context_and_rebinds_every_index_and_history_leaf() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        let (cached, cached_read) = writer_state().await;
        let (fresh, fresh_read) = writer_state().await;
        let final_block = fixture(2);
        let mut proposal = (*final_block).clone();
        let header = Arc::make_mut(&mut proposal.header);
        header.nonce = [0; 32].into();
        header.solution = equihash::Solution::Common([0; 1344]);
        let proposal = Arc::new(proposal);
        let proposal_hash = proposal.hash();
        cached
            .clone()
            .oneshot(crate::Request::CheckBlockProposalValidity(proposal.into()))
            .await
            .unwrap();
        assert_eq!(cached_read.mining_state.borrow().full_contextual_checks, 1);
        let mut updates = cached_read.mining_tip.subscribe();
        let crate::Response::MiningStaged(Some(guard)) = cached
            .clone()
            .oneshot(crate::Request::AdmitPreparedMiningBlock(
                final_block.clone(),
            ))
            .await
            .unwrap()
        else {
            panic!("exact completed proposal must admit on the writer")
        };
        assert_eq!(
            cached_read.mining_state.borrow().contextual_admission_hits,
            1
        );
        assert_eq!(cached_read.mining_state.borrow().full_contextual_checks, 1);
        assert_eq!(
            *updates.borrow_and_update(),
            Some((block::Height(2), final_block.hash()))
        );
        let crate::Response::ReusableBlockProposal(Some(prepared)) = cached
            .clone()
            .oneshot(crate::Request::ReusableBlockProposal(final_block.clone()))
            .await
            .unwrap()
        else {
            panic!("completed exact proposal must return its semantic result")
        };
        cached
            .clone()
            .oneshot(crate::Request::CommitSemanticallyVerifiedBlock(prepared))
            .await
            .unwrap();
        assert!(
            updates.has_changed().unwrap(),
            "speculative-to-identical-validated parent must notify"
        );
        assert_eq!(
            *updates.borrow_and_update(),
            Some((block::Height(2), final_block.hash()))
        );
        assert!(cached_read.mining_state.borrow().staged.is_none());
        assert_eq!(
            cached_read.mining_state.borrow().validated_tip,
            Some((block::Height(2), final_block.hash()))
        );
        assert_eq!(cached_read.mining_state.borrow().full_contextual_checks, 1);
        assert_eq!(cached_read.mining_state.borrow().contextual_commit_hits, 1);
        drop(guard);
        fresh
            .clone()
            .oneshot(crate::Request::CommitSemanticallyVerifiedBlock(
                final_block.clone().into(),
            ))
            .await
            .unwrap();
        assert_eq!(fresh_read.mining_state.borrow().full_contextual_checks, 1);
        let cached_chain = cached_read.latest_best_chain().unwrap();
        let fresh_chain = fresh_read.latest_best_chain().unwrap();
        // Chain's own equality only compares chain ordering: compare every application artifact.
        assert!(cached_chain.eq_internal_state(&fresh_chain));
        assert_eq!(
            cached_chain.height_by_hash.get(&final_block.hash()),
            Some(&block::Height(2))
        );
        assert!(!cached_chain.height_by_hash.contains_key(&proposal_hash));
        assert_eq!(
            cached_chain.history_trees_by_height[&block::Height(2)].hash(),
            fresh_chain.history_trees_by_height[&block::Height(2)].hash()
        );
        for tx in &final_block.transactions {
            assert!(cached_chain.tx_loc_by_hash.contains_key(&tx.hash()));
        }
    })
    .await
    .expect("contextual reuse parity test must not stall");
}

#[tokio::test(flavor = "multi_thread")]
async fn private_parent_preflight_never_authorizes_commit_or_outlives_its_reservation() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        let (state, read) = writer_state().await;
        let parent = fixture(2);
        state
            .clone()
            .oneshot(crate::Request::CheckBlockProposalValidity(
                parent.clone().into(),
            ))
            .await
            .unwrap();
        let permit = read
            .mining_admission_slot
            .clone()
            .try_acquire_owned()
            .unwrap();
        assert!(matches!(
            read.clone()
                .oneshot(crate::ReadRequest::StageMiningBlock(parent.clone().into()))
                .await
                .unwrap(),
            crate::ReadResponse::MiningStaged(None)
        ));
        assert!(read.mining_state.borrow().staged.is_none());
        drop(permit);
        let crate::Response::MiningStaged(Some(guard)) = state
            .clone()
            .oneshot(crate::Request::StageMiningBlock(parent.clone().into()))
            .await
            .unwrap()
        else {
            panic!("a valid direct child must stage privately")
        };
        let crate::ReadResponse::ChainInfo(info) = read
            .clone()
            .oneshot(crate::ReadRequest::MiningChainInfo)
            .await
            .unwrap()
        else {
            panic!("wrong mining context response")
        };
        let mut child = (*fixture(3)).clone();
        Arc::make_mut(&mut child.header).commitment_bytes =
            <[u8; 32]>::from(info.chain_history_root.unwrap()).into();
        let child = Arc::new(child);
        state
            .clone()
            .oneshot(crate::Request::CheckBlockProposalValidity(
                child.clone().into(),
            ))
            .await
            .unwrap();
        assert_eq!(read.mining_state.borrow().proposals.len(), 1);
        assert!(matches!(
            state
                .clone()
                .oneshot(crate::Request::ReusableBlockProposalWithWorkId {
                    block: child.clone(),
                    work_id: proposal_key(&child),
                })
                .await
                .unwrap(),
            crate::Response::ReusableBlockProposal(None)
        ));
        assert!(matches!(
            state
                .clone()
                .oneshot(crate::Request::AdmitPreparedMiningBlock(child.clone()))
                .await
                .unwrap(),
            crate::Response::MiningStaged(None)
        ));
        assert_eq!(
            read.clone().oneshot(crate::ReadRequest::Tip).await.unwrap(),
            crate::ReadResponse::Tip(Some((block::Height(1), fixture(1).hash())))
        );
        for candidate in [&parent, &child] {
            assert_eq!(
                read.clone()
                    .oneshot(crate::ReadRequest::Block(candidate.hash().into()))
                    .await
                    .unwrap(),
                crate::ReadResponse::Block(None)
            );
        }
        // The private fork still checks the history commitment, rather than trusting the parent.
        assert!(state
            .clone()
            .oneshot(crate::Request::CheckBlockProposalValidity(
                fixture(3).into()
            ))
            .await
            .is_err());
        read.mining_state.send_modify(|mining| {
            Arc::make_mut(mining.staged.as_mut().unwrap()).expires = Instant::now();
        });
        assert!(state
            .clone()
            .oneshot(crate::Request::CheckBlockProposalValidity(
                child.clone().into()
            ))
            .await
            .is_err());
        drop(guard);
        state
            .clone()
            .oneshot(crate::Request::CommitSemanticallyVerifiedBlock(
                parent.into(),
            ))
            .await
            .unwrap();
        // Only an ordinary validated-parent preflight can populate the reusable verdict.
        state
            .clone()
            .oneshot(crate::Request::CheckBlockProposalValidity(
                child.clone().into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            state
                .oneshot(crate::Request::ReusableBlockProposal(child))
                .await
                .unwrap(),
            crate::Response::ReusableBlockProposal(Some(_))
        ));
    })
    .await
    .expect("private-parent preflight test must not stall");
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_cache_result_cannot_bypass_a_writer_invalidation_epoch() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        let (state, read) = writer_state().await;
        let child = fixture(2);
        state
            .clone()
            .oneshot(crate::Request::CheckBlockProposalValidity(
                child.clone().into(),
            ))
            .await
            .unwrap();
        let crate::Response::ReusableBlockProposal(Some(saved)) = state
            .clone()
            .oneshot(crate::Request::ReusableBlockProposal(child.clone()))
            .await
            .unwrap()
        else {
            panic!("proposal must cache")
        };
        let old_generation = read.mining_state.borrow().generation;
        let mut updates = read.mining_tip.subscribe();
        // The attempted operator mutation leaves the validated parent unchanged, but retires
        // the writer epoch. A result acquired before it must not authorize contextual reuse.
        assert!(state
            .clone()
            .oneshot(crate::Request::InvalidateBlock(fixture(0).hash()))
            .await
            .is_err());
        assert!(read.mining_state.borrow().generation > old_generation);
        assert!(
            updates.has_changed().unwrap(),
            "generation-only writes must wake mining preflight"
        );
        assert_eq!(
            *updates.borrow_and_update(),
            Some((block::Height(1), fixture(1).hash()))
        );
        state
            .oneshot(crate::Request::CommitSemanticallyVerifiedBlock(saved))
            .await
            .unwrap();
        assert_eq!(read.mining_state.borrow().contextual_commit_hits, 0);
        assert_eq!(read.mining_state.borrow().full_contextual_checks, 2);
        assert_eq!(
            read.clone().oneshot(crate::ReadRequest::Tip).await.unwrap(),
            crate::ReadResponse::Tip(Some((block::Height(2), child.hash())))
        );
    })
    .await
    .expect("writer epoch invalidation test must not stall");
}

#[tokio::test(flavor = "multi_thread")]
async fn writer_cache_retains_multiple_proposals_and_has_a_fixed_bound() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        let (state, read) = writer_state().await;
        let mut candidates = Vec::new();
        for offset in 0..MAX_PROPOSALS + 2 {
            let mut candidate = (*fixture(2)).clone();
            Arc::make_mut(&mut candidate.header).time +=
                chrono::Duration::seconds(i64::try_from(offset).unwrap());
            let candidate = Arc::new(candidate);
            state
                .clone()
                .oneshot(crate::Request::CheckBlockProposalValidity(
                    candidate.clone().into(),
                ))
                .await
                .unwrap();
            candidates.push(candidate);
        }
        assert_eq!(read.mining_state.borrow().proposals.len(), MAX_PROPOSALS);
        for (index, candidate) in candidates.into_iter().enumerate() {
            let crate::Response::ReusableBlockProposal(result) = state
                .clone()
                .oneshot(crate::Request::ReusableBlockProposal(candidate))
                .await
                .unwrap()
            else {
                panic!("wrong cache response")
            };
            assert_eq!(result.is_some(), index >= 2);
        }
    })
    .await
    .expect("bounded proposal cache test must not stall");
}
