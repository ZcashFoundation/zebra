//! Test getblocktemplate RPC method.
//!
//! This test requires a cached chain state that is partially synchronized close to the
//! network chain tip height. It will finish the sync and update the cached chain state.
//!
//! After finishing the sync, it will call getblocktemplate.

use std::time::Duration;

use color_eyre::eyre::{eyre, Context, Result};

use futures::FutureExt;

use zebra_chain::{
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashSerialize,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::methods::get_block_template_rpcs::{
    get_block_template::{
        proposal::TimeSource, GetBlockTemplate, JsonParameters, ProposalResponse,
    },
    types::get_block_template::proposal_block_from_template,
};

use crate::common::{
    launch::{can_spawn_zebrad_for_test_type, spawn_zebrad_for_rpc},
    sync::{check_sync_logs_until, MempoolBehavior, SYNC_FINISHED_REGEX},
    test_type::TestType,
};

/// Delay between getting block proposal results and cancelling long poll requests.
///
/// This ensures that a new template can be deserialized and sent to interrupt the
/// block proposal requests if the old template is no longer valid in edge-cases where
/// an old template becomes invalid right after it's returned. We've seen the getblocktemplate
/// respond within ~50ms of a request locally, and this test is run on GCP compute instances
/// that should offer comparable latency in CI.
pub const EXTRA_LONGPOLL_WAIT_TIME: Duration = Duration::from_millis(150);

/// Delay between attempts to validate a template as block proposals.
///
/// Running many iterations in short intervals tests that long poll requests correctly
/// return `submit_old: false` when the old template becomes invalid.
///
/// We've seen a typical call to `try_validate_block_template` take ~900ms, a minimum
/// spacing of 1s ensures that this test samples various chain states and mempool contents.
pub const BLOCK_PROPOSAL_INTERVAL: Duration = Duration::from_secs(1);

/// Number of times we want to try submitting a block template as a block proposal at an interval
/// that allows testing the varying mempool contents.
///
/// We usually see at least 1 template with a `submit_old` of false from a long poll request
/// with 1000 `try_validate_block_template` calls.
///
/// The block proposal portion of this test should take ~18 minutes for 1000 iterations at
/// an interval of 1s.
///
/// See [`BLOCK_PROPOSAL_INTERVAL`] for more information.
const NUM_SUCCESSFUL_BLOCK_PROPOSALS_REQUIRED: usize = 1000;

/// Launch Zebra, wait for it to sync, and check the getblocktemplate RPC returns without errors.
pub(crate) async fn run() -> Result<()> {
    let _init_guard = zebra_test::init();

    // We want a zebra state dir in place,
    let test_type = TestType::UpdateZebraCachedStateWithRpc;
    let test_name = "get_block_template_test";
    let network = Network::Mainnet;

    // Skip the test unless the user specifically asked for it and there is a zebrad_state_path
    if !can_spawn_zebrad_for_test_type(test_name, test_type, true) {
        return Ok(());
    }

    tracing::info!(
        ?network,
        ?test_type,
        "running getblocktemplate test using zebrad",
    );

    let should_sync = true;
    let (zebrad, zebra_rpc_address) =
        spawn_zebrad_for_rpc(network.clone(), test_name, test_type, should_sync)?
            .ok_or_else(|| eyre!("getblocktemplate test requires a cached state"))?;

    let rpc_address = zebra_rpc_address.expect("test type must have RPC port");

    let mut zebrad = check_sync_logs_until(
        zebrad,
        &network,
        SYNC_FINISHED_REGEX,
        MempoolBehavior::ShouldAutomaticallyActivate,
        true,
    )?;

    let client = RpcRequestClient::new(rpc_address);

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

    let mut response_text = getblocktemplate_response.text().await?;
    // This string can be extremely long in logs.
    if response_text.len() > 1003 {
        let end = response_text.len() - 500;
        // Replace the middle bytes with "...", but leave 500 bytes on either side.
        // The response text is ascii, so this replacement won't panic.
        response_text.replace_range(500..=end, "...");
    }

    tracing::info!(
        response_text,
        "got getblocktemplate response, might not have transactions"
    );

    assert!(is_response_success);

    for _ in 0..NUM_SUCCESSFUL_BLOCK_PROPOSALS_REQUIRED {
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

/// Accepts an [`RpcRequestClient`], calls getblocktemplate in template mode,
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
async fn try_validate_block_template(client: &RpcRequestClient) -> Result<()> {
    let mut response_json_result: GetBlockTemplate = client
        .json_result_from_call("getblocktemplate", "[]")
        .await
        .expect("response should be success output with a serialized `GetBlockTemplate`");

    let (long_poll_result_tx, mut long_poll_result_rx) =
        tokio::sync::watch::channel(response_json_result.clone());
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel(1);

    {
        let client = client.clone();
        let mut long_poll_id = response_json_result.long_poll_id;

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
                        long_poll_id = long_poll_result.long_poll_id;

                        if let Some(false) = long_poll_result.submit_old {
                            let _ = long_poll_result_tx.send(long_poll_result);
                        }
                    }
                }
            }
        });
    };

    loop {
        let mut proposal_requests = vec![];

        for time_source in TimeSource::valid_sources() {
            // Propose a new block with an empty solution and nonce field

            let raw_proposal_block = hex::encode(
                proposal_block_from_template(
                    &response_json_result,
                    time_source,
                    NetworkUpgrade::Nu5,
                )?
                .zcash_serialize_to_vec()?,
            );
            let template = response_json_result.clone();

            proposal_requests.push(async move {
                (
                    client
                        .json_result_from_call(
                            "getblocktemplate",
                            format!(r#"[{{"mode":"proposal","data":"{raw_proposal_block}"}}]"#),
                        )
                        .await,
                    template,
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
                for (proposal_result, template, time_source) in proposal_results {
                    let proposal_result = proposal_result
                        .expect("response should be success output with a serialized `ProposalResponse`");

                    if let ProposalResponse::Rejected(reject_reason) = proposal_result {
                        tracing::info!(
                            ?reject_reason,
                            ?template,
                            ?time_source,
                            "failed to validate template as a block proposal"
                        );

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
