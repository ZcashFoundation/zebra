//! Simulates a full Zebra node’s block‐processing pipeline on a predefined Orchard/ZSA workflow.
//!
//! This integration test reads a sequence of serialized regtest blocks (including Orchard burns
//! and ZSA issuance), feeds them through the node’s deserialization, consensus router, and state
//! service exactly as if they arrived from the network, and verifies that each block is accepted
//! (or fails at the injected point).
//!
//! In a future PR, we will add tracking and verification of issuance/burn state changes so that
//! the test can also assert that on-chain asset state (total supply and finalization flags)
//! matches the expected values computed in memory.
//!
//! In short, it demonstrates end-to-end handling of Orchard asset burns and ZSA issuance through
//! consensus (with state verification to follow in the next PR).

use std::{
    collections::{hash_map, HashMap},
    sync::Arc,
};

use color_eyre::eyre::Report;
use tower::ServiceExt;

use orchard::{
    asset_record::AssetRecord, issuance::IssueAction, keys::IssuanceValidatingKey, note::AssetBase,
    value::NoteValue,
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
    vectors::{OrchardWorkflowBlock, ORCHARD_WORKFLOW_BLOCKS_ZSA},
};

use crate::{block::Request, Config};

type AssetRecords = HashMap<AssetBase, AssetRecord>;

type TranscriptItem = (Request, Result<Hash, ExpectedTranscriptError>);

#[derive(Debug)]
enum AssetRecordsError {
    BurnAssetMissing,
    EmptyActionNotFinalized,
    AmountOverflow,
    MissingRefNote,
    ModifyFinalized,
}

/// Processes orchard burns, decreasing asset supply.
fn process_burns<'a, I: Iterator<Item = &'a BurnItem>>(
    asset_records: &mut AssetRecords,
    burns: I,
) -> Result<(), AssetRecordsError> {
    for burn in burns {
        // FIXME: check for burn specific errors?
        let asset_record = asset_records
            .get_mut(&burn.asset())
            .ok_or(AssetRecordsError::BurnAssetMissing)?;

        asset_record.amount = NoteValue::from_raw(
            asset_record
                .amount
                .inner()
                .checked_sub(burn.amount().inner())
                .ok_or(AssetRecordsError::AmountOverflow)?,
        );
    }

    Ok(())
}

/// Processes orchard issue actions, increasing asset supply.
fn process_issue_actions<'a, I: Iterator<Item = &'a IssueAction>>(
    asset_records: &mut AssetRecords,
    ik: &IssuanceValidatingKey,
    actions: I,
) -> Result<(), AssetRecordsError> {
    for action in actions {
        let action_asset = AssetBase::derive(ik, action.asset_desc_hash());
        let reference_note = action.get_reference_note();
        let is_finalized = action.is_finalized();

        let mut note_amounts = action.notes().iter().map(|note| {
            if note.asset() == action_asset {
                Ok(note.value())
            } else {
                Err(AssetRecordsError::BurnAssetMissing)
            }
        });

        let first_note_amount = match note_amounts.next() {
            Some(note_amount) => note_amount,
            None => {
                if is_finalized {
                    Ok(NoteValue::from_raw(0))
                } else {
                    Err(AssetRecordsError::EmptyActionNotFinalized)
                }
            }
        };

        for amount_result in std::iter::once(first_note_amount).chain(note_amounts) {
            let amount = amount_result?;

            // FIXME: check for issuance specific errors?
            match asset_records.entry(action_asset) {
                hash_map::Entry::Occupied(mut entry) => {
                    let asset_record = entry.get_mut();
                    asset_record.amount =
                        (asset_record.amount + amount).ok_or(AssetRecordsError::AmountOverflow)?;
                    if asset_record.is_finalized {
                        return Err(AssetRecordsError::ModifyFinalized);
                    }
                    asset_record.is_finalized = is_finalized;
                }

                hash_map::Entry::Vacant(entry) => {
                    entry.insert(AssetRecord {
                        amount,
                        is_finalized,
                        reference_note: *reference_note.ok_or(AssetRecordsError::MissingRefNote)?,
                    });
                }
            }
        }
    }

    Ok(())
}

/// Builds assets records for the given blocks.
fn build_asset_records<'a, I: IntoIterator<Item = &'a TranscriptItem>>(
    blocks: I,
) -> Result<AssetRecords, AssetRecordsError> {
    blocks
        .into_iter()
        .filter_map(|(request, result)| match (request, result) {
            (Request::Commit(block), Ok(_)) => Some(&block.transactions),
            _ => None,
        })
        .flatten()
        .try_fold(HashMap::new(), |mut asset_records, tx| {
            process_burns(&mut asset_records, tx.orchard_burns())?;

            if let Some(issue_data) = tx.orchard_issue_data() {
                process_issue_actions(
                    &mut asset_records,
                    issue_data.inner().ik(),
                    issue_data.actions(),
                )?;
            }

            Ok(asset_records)
        })
}

/// Creates transcript data from predefined workflow blocks.
fn create_transcript_data<'a, I: IntoIterator<Item = &'a OrchardWorkflowBlock>>(
    serialized_blocks: I,
) -> impl Iterator<Item = TranscriptItem> + use<'a, I> {
    let workflow_blocks =
        serialized_blocks
            .into_iter()
            .map(|OrchardWorkflowBlock { bytes, is_valid }| {
                (
                    Arc::new(
                        Block::zcash_deserialize(&bytes[..]).expect("block should deserialize"),
                    ),
                    *is_valid,
                )
            });

    std::iter::once((regtest_genesis_block(), true))
        .chain(workflow_blocks)
        .map(|(block, is_valid)| {
            (
                Request::Commit(block.clone()),
                if is_valid {
                    Ok(block.hash())
                } else {
                    Err(ExpectedTranscriptError::Any)
                },
            )
        })
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
async fn check_orchard_zsa_workflow() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Some(1), Some(1), Some(1));

    let (state_service, read_state_service, _, _) = zebra_state::init_test_services(&network);

    let (block_verifier_router, _tx_verifier, _groth16_download_handle, _max_checkpoint_height) =
        crate::router::init(Config::default(), &network, state_service.clone()).await;

    let transcript_data =
        create_transcript_data(ORCHARD_WORKFLOW_BLOCKS_ZSA.iter()).collect::<Vec<_>>();

    let asset_records =
        build_asset_records(&transcript_data).expect("should calculate asset_records");

    // Before applying the blocks, ensure that none of the assets exist in the state.
    for &asset_base in asset_records.keys() {
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
    for (&asset_base, asset_record) in &asset_records {
        let asset_state = request_asset_state(&read_state_service, asset_base)
            .await
            .expect("State should contain this asset now.");

        assert_eq!(
            asset_state.is_finalized, asset_record.is_finalized,
            "Finalized state does not match for asset {:?}.",
            asset_base
        );

        assert_eq!(
            asset_state.total_supply,
            // FIXME: Fix it after chaning ValueSum to NoteValue in AssetSupply in orchard
            u64::try_from(i128::from(asset_record.amount))
                .expect("asset supply amount should be within u64 range"),
            "Total supply mismatch for asset {:?}.",
            asset_base
        );
    }

    Ok(())
}
