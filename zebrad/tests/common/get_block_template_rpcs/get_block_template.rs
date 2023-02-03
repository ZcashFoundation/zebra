//! Test getblocktemplate RPC method.
//!
//! This test requires a cached chain state that is partially synchronized close to the
//! network chain tip height. It will finish the sync and update the cached chain state.
//!
//! After finishing the sync, it will call getblocktemplate.

use std::time::Duration;

use color_eyre::eyre::{eyre, Context, Result};

use futures::FutureExt;
use zebra_chain::{parameters::Network, serialization::ZcashSerialize};
use zebra_rpc::methods::get_block_template_rpcs::{
    get_block_template::{
        proposal::TimeSource, GetBlockTemplate, JsonParameters, ProposalResponse,
    },
    types::get_block_template::proposal_block_from_template,
};

use crate::common::{
    launch::{can_spawn_zebrad_for_rpc, spawn_zebrad_for_rpc},
    rpc_client::RPCRequestClient,
    sync::{check_sync_logs_until, MempoolBehavior, SYNC_FINISHED_REGEX},
    test_type::TestType,
};

/// How long the test waits for the mempool to download and verify transactions.
///
/// We've seen it take anywhere from 1-45 seconds for the mempool to have some transactions in it.
pub const EXPECTED_MEMPOOL_TRANSACTION_TIME: Duration = Duration::from_secs(45);

/// Delay between getting block proposal results and cancelling long poll requests.
pub const EXTRA_LONGPOLL_WAIT_TIME: Duration = Duration::from_millis(150);

/// Delay between attempts to validate a template as block proposals.
///
/// Running many iterations in short intervals tests that long poll requests correctly
/// return `submit_old: false` when the old template becomes invalid.
pub const BLOCK_PROPOSAL_INTERVAL: Duration = Duration::from_millis(300);

/// Number of times we want to try submitting a block template as a block proposal at an interval
/// that allows testing the varying mempool contents.
const NUM_SUCCESSFUL_BLOCK_PROPOSALS_REQUIRED: usize = 1000;

/// Launch Zebra, wait for it to sync, and check the getblocktemplate RPC returns without errors.
pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a zebra state dir in place,
    let test_type = TestType::UpdateZebraCachedStateWithRpc;
    let test_name = "get_block_template_test";
    let network = Network::Mainnet;

    // Skip the test unless the user specifically asked for it and there is a zebrad_state_path
    if !can_spawn_zebrad_for_rpc(test_name, test_type) {
        return Ok(());
    }

    tracing::info!(
        ?network,
        ?test_type,
        "running getblocktemplate test using zebrad",
    );

    let should_sync = true;
    let (zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc(network, test_name, test_type, should_sync)?
            .ok_or_else(|| eyre!("getblocktemplate test requires a cached state"))?;

    let rpc_address = zebra_rpc_address.expect("test type must have RPC port");

    let mut zebrad = check_sync_logs_until(
        zebrad,
        network,
        SYNC_FINISHED_REGEX,
        MempoolBehavior::ShouldAutomaticallyActivate,
        true,
    )?;

    let client = RPCRequestClient::new(rpc_address);

    tracing::info!(
        "calling getblocktemplate RPC method at {rpc_address}, \
         with a mempool that is likely empty...",
    );
    let getblocktemplate_response = client
        .call(
            "getblocktemplate",
            // test that unknown capabilities are parsed as valid input
            "[{\"capabilities\": [\"generation\"]}]".to_string(),
        )
        .await?;

    let is_response_success = getblocktemplate_response.status().is_success();
    let response_text = getblocktemplate_response.text().await?;

    tracing::info!(
        response_text,
        "got getblocktemplate response, might not have transactions"
    );

    assert!(is_response_success);

    tokio::time::sleep(EXPECTED_MEMPOOL_TRANSACTION_TIME).await;

    for _ in 0..NUM_SUCCESSFUL_BLOCK_PROPOSALS_REQUIRED {
        tracing::info!(
            "waiting {EXPECTED_MEMPOOL_TRANSACTION_TIME:?} for the mempool \
             to download and verify some transactions...",
        );

        tokio::time::sleep(BLOCK_PROPOSAL_INTERVAL).await;

        tracing::info!(
            "calling getblocktemplate RPC method at {rpc_address}, \
                 with a mempool that likely has transactions and attempting \
                 to validate response result as a block proposal",
        );

        let (validation_result, _) = futures::future::join(
            try_validate_block_template(&client),
            tokio::time::sleep(BLOCK_PROPOSAL_INTERVAL),
        )
        .await;

        validation_result.expect("block proposal validation failed");
    }

    zebrad.kill(false)?;

    let output = zebrad.wait_with_output()?;
    let output = output.assert_failure()?;

    // [Note on port conflict](#Note on port conflict)
    output
        .assert_was_killed()
        .wrap_err("Possible port conflict. Are there other acceptance tests running?")
}

/// Accepts an [`RPCRequestClient`], calls getblocktemplate in template mode,
/// deserializes and transforms the block template in the response into block proposal data,
/// then calls getblocktemplate RPC in proposal mode with the serialized and hex-encoded data.
///
/// Returns an error if it fails to transform template to block proposal or serialize the block proposal
/// Returns `Ok(())` if the block proposal is valid or an error with the reject-reason if the result is
/// an `ErrorResponse`.
///
/// ## Panics
///
/// If an RPC call returns a failure
/// If the response result cannot be deserialized to `GetBlockTemplate` in 'template' mode
/// or `ProposalResponse` in 'proposal' mode.
async fn try_validate_block_template(client: &RPCRequestClient) -> Result<()> {
    let mut response_json_result: GetBlockTemplate = client
        .json_result_from_call("getblocktemplate", "[]".to_string())
        .await
        .expect("response should be success output with a serialized `GetBlockTemplate`");

    let (long_poll_result_tx, mut long_poll_result_rx) =
        tokio::sync::watch::channel(response_json_result.clone());
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel(1);

    {
        let client = client.clone();
        let mut long_poll_id = response_json_result.long_poll_id.clone();

        tokio::spawn(async move {
            loop {
                let long_poll_request = async {
                    let long_poll_json_params = serde_json::to_string(&vec![JsonParameters {
                        long_poll_id: Some(long_poll_id),
                        ..Default::default()
                    }])
                    .expect("JsonParameters should serialize successfully");

                    let result: GetBlockTemplate = client
                            .json_result_from_call("getblocktemplate", long_poll_json_params)
                            .await
                            .expect(
                                "response should be success output with a serialized `GetBlockTemplate`",
                            );

                    result
                };

                tokio::select! {
                    _ = done_rx.recv() => {
                        break;
                    }

                    long_poll_result = long_poll_request => {
                        long_poll_id = long_poll_result.long_poll_id.clone();

                        if let Some(false) = long_poll_result.submit_old {
                            let _ = long_poll_result_tx.send(long_poll_result);
                        }
                    }
                }
            }
        });
    };

    loop {
        tracing::info!(
            ?response_json_result,
            "got getblocktemplate response, hopefully with transactions"
        );

        let mut proposal_requests = vec![];

        for time_source in TimeSource::valid_sources() {
            // Propose a new block with an empty solution and nonce field
            tracing::info!(
                "calling getblocktemplate with a block proposal and time source {time_source:?}...",
            );

            let raw_proposal_block = hex::encode(
                proposal_block_from_template(&response_json_result, time_source)?
                    .zcash_serialize_to_vec()?,
            );

            proposal_requests.push(async move {
                (
                    client
                        .json_result_from_call(
                            "getblocktemplate",
                            format!(r#"[{{"mode":"proposal","data":"{raw_proposal_block}"}}]"#),
                        )
                        .await,
                    time_source,
                )
            });
        }

        tokio::select! {
            Ok(()) = long_poll_result_rx.changed() => {
                tracing::info!("got longpolling response with submitold of false before result of proposal tests");

                // The task that handles the long polling request will keep checking for
                // a new template with `submit_old`: false
                response_json_result = long_poll_result_rx.borrow().clone();

                continue;
            },

            proposal_results = futures::future::join_all(proposal_requests).then(|results| async move {
                tokio::time::sleep(EXTRA_LONGPOLL_WAIT_TIME).await;
                results
            }) => {
                let _ = done_tx.send(()).await;
                for (proposal_result, time_source) in proposal_results {
                    let proposal_result = proposal_result
                        .expect("response should be success output with with a serialized `ProposalResponse`");

                    tracing::info!(
                        ?proposal_result,
                        ?time_source,
                        "got getblocktemplate proposal response"
                    );

                    if let ProposalResponse::Rejected(reject_reason) = proposal_result {
                        Err(eyre!(
                            "unsuccessful block proposal validation, reason: {reject_reason:?}"
                        ))?;
                    } else {
                        assert_eq!(ProposalResponse::Valid, proposal_result);
                    }
                }

                break;
            }
        }
    }

    Ok(())
}
