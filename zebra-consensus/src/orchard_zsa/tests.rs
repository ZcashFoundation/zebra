// FIXME: consider merging it with router/tests.rs

use std::sync::Arc;

use color_eyre::eyre::Report;
use tower::ServiceExt;

use orchard::{
    issuance::Error as IssuanceError,
    issuance::IssueAction,
    note::AssetBase,
    supply_info::{AssetSupply, SupplyInfo},
    value::ValueSum,
};

use zebra_chain::{
    block::{genesis::regtest_genesis_block, Block, Hash},
    orchard_zsa::{AssetState, BurnItem},
    parameters::Network,
    serialization::ZcashDeserialize,
};

use zebra_state::{ReadRequest, ReadResponse, ReadStateService};

use zebra_test::{
    transcript::{ExpectedTranscriptError, Transcript},
    vectors::ORCHARD_ZSA_WORKFLOW_BLOCKS,
};

use crate::{block::Request, Config};

type TranscriptItem = (Request, Result<Hash, ExpectedTranscriptError>);

/// Processes orchard burns, decreasing asset supply.
fn process_burns<'a, I: Iterator<Item = &'a BurnItem>>(
    supply_info: &mut SupplyInfo,
    burns: I,
) -> Result<(), IssuanceError> {
    for burn in burns {
        // Burns reduce supply, so negate the amount.
        let amount = (-ValueSum::from(burn.amount())).ok_or(IssuanceError::ValueSumOverflow)?;

        supply_info.add_supply(
            burn.asset(),
            AssetSupply {
                amount,
                is_finalized: false,
            },
        )?;
    }

    Ok(())
}

/// Processes orchard issue actions, increasing asset supply.
fn process_issue_actions<'a, I: Iterator<Item = &'a IssueAction>>(
    supply_info: &mut SupplyInfo,
    issue_actions: I,
) -> Result<(), IssuanceError> {
    for action in issue_actions {
        let is_finalized = action.is_finalized();

        for note in action.notes() {
            supply_info.add_supply(
                note.asset(),
                AssetSupply {
                    amount: note.value().into(),
                    is_finalized,
                },
            )?;
        }
    }

    Ok(())
}

/// Calculates supply info for all assets in the given blocks.
fn calc_asset_supply_info<'a, I: IntoIterator<Item = &'a TranscriptItem>>(
    blocks: I,
) -> Result<SupplyInfo, IssuanceError> {
    blocks
        .into_iter()
        .filter_map(|(request, _)| match request {
            Request::Commit(block) => Some(&block.transactions),
            #[cfg(feature = "getblocktemplate-rpcs")]
            Request::CheckProposal(_) => None,
        })
        .flatten()
        .try_fold(SupplyInfo::new(), |mut supply_info, tx| {
            process_burns(&mut supply_info, tx.orchard_burns().iter())?;
            process_issue_actions(&mut supply_info, tx.orchard_issue_actions())?;
            Ok(supply_info)
        })
}

/// Creates transcript data from predefined workflow blocks.
fn create_transcript_data<'a, I: IntoIterator<Item = &'a Vec<u8>>>(
    serialized_blocks: I,
) -> impl Iterator<Item = TranscriptItem> + use<'a, I> {
    let workflow_blocks = serialized_blocks.into_iter().map(|block_bytes| {
        Arc::new(Block::zcash_deserialize(&block_bytes[..]).expect("block should deserialize"))
    });

    std::iter::once(regtest_genesis_block())
        .chain(workflow_blocks)
        .map(|block| (Request::Commit(block.clone()), Ok(block.hash())))
}

/// Queries the state service for the asset state of the given asset.
async fn request_asset_state(
    read_state_service: &ReadStateService,
    asset_base: AssetBase,
) -> Option<AssetState> {
    let request = ReadRequest::AssetState {
        asset_base,
        include_non_finalized: true,
    };

    match read_state_service.clone().oneshot(request).await {
        Ok(ReadResponse::AssetState(asset_state)) => asset_state,
        _ => unreachable!("The state service returned an unexpected response."),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn check_zsa_workflow() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Some(1), Some(1), Some(1));

    let (state_service, read_state_service, _, _) = zebra_state::init_test_services(&network);

    let (block_verifier_router, _tx_verifier, _groth16_download_handle, _max_checkpoint_height) =
        crate::router::init(Config::default(), &network, state_service.clone()).await;

    let transcript_data =
        create_transcript_data(ORCHARD_ZSA_WORKFLOW_BLOCKS.iter()).collect::<Vec<_>>();

    let asset_supply_info =
        calc_asset_supply_info(&transcript_data).expect("should calculate asset_supply_info");

    // Before applying the blocks, ensure that none of the assets exist in the state.
    for (&asset_base, _asset_supply) in &asset_supply_info.assets {
        assert!(
            request_asset_state(&read_state_service, asset_base)
                .await
                .is_none(),
            "State should initially have no info about this asset."
        );
    }

    // Verify all blocks in the transcript against the consensus and the state.
    Transcript::from(transcript_data)
        .check(block_verifier_router.clone())
        .await?;

    // After processing the transcript blocks, verify that the state matches the expected supply info.
    for (&asset_base, asset_supply) in &asset_supply_info.assets {
        let asset_state = request_asset_state(&read_state_service, asset_base)
            .await
            .expect("State should contain this asset now.");

        assert_eq!(
            asset_state.is_finalized, asset_supply.is_finalized,
            "Finalized state does not match for asset {:?}.",
            asset_base
        );

        assert_eq!(
            asset_state.total_supply,
            u64::try_from(i128::from(asset_supply.amount))
                .expect("asset supply amount should be within u64 range"),
            "Total supply mismatch for asset {:?}.",
            asset_base
        );
    }

    Ok(())
}
