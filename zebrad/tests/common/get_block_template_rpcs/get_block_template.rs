//! Test getblocktemplate RPC method.
//!
//! This test requires a cached chain state that is partially synchronized close to the
//! network chain tip height. It will finish the sync and update the cached chain state.
//!
//! After finishing the sync, it will call getblocktemplate.

use std::time::Duration;

use color_eyre::eyre::{eyre, Context, Result};

use zebra_chain::{parameters::Network, serialization::ZcashSerialize};
use zebra_rpc::methods::get_block_template_rpcs::{
    get_block_template::{proposal::TimeSource, ProposalResponse},
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

    tracing::info!(
        "waiting {EXPECTED_MEMPOOL_TRANSACTION_TIME:?} for the mempool \
         to download and verify some transactions...",
    );

    tokio::time::sleep(EXPECTED_MEMPOOL_TRANSACTION_TIME).await;

    /* TODO: activate this test after #5925 and #5953 have merged,
             and we've checked for any other bugs using #5944.
    tracing::info!(
        "calling getblocktemplate RPC method at {rpc_address}, \
             with a mempool that likely has transactions and attempting \
             to validate response result as a block proposal",
    );

    try_validate_block_template(&client)
        .await
        .expect("block proposal validation failed");
     */

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
#[allow(dead_code)]
async fn try_validate_block_template(client: &RPCRequestClient) -> Result<()> {
    let response_json_result = client
        .json_result_from_call("getblocktemplate", "[]".to_string())
        .await
        .expect("response should be success output with with a serialized `GetBlockTemplate`");

    tracing::info!(
        ?response_json_result,
        "got getblocktemplate response, hopefully with transactions"
    );

    // Propose a new block with an empty solution and nonce field
    tracing::info!("calling getblocktemplate with a block proposal...",);

    // TODO: update this to use all valid time sources in the next PR
    #[allow(clippy::single_element_loop)]
    for proposal_block in [proposal_block_from_template(
        response_json_result,
        TimeSource::CurTime,
    )?] {
        let raw_proposal_block = hex::encode(proposal_block.zcash_serialize_to_vec()?);

        let json_result = client
            .json_result_from_call(
                "getblocktemplate",
                format!(r#"[{{"mode":"proposal","data":"{raw_proposal_block}"}}]"#),
            )
            .await
            .expect("response should be success output with with a serialized `ProposalResponse`");

        tracing::info!(
            ?json_result,
            ?proposal_block.header.time,
            "got getblocktemplate proposal response"
        );

        if let ProposalResponse::Rejected(reject_reason) = json_result {
            Err(eyre!(
                "unsuccessful block proposal validation, reason: {reject_reason:?}"
            ))?;
        } else {
            assert_eq!(ProposalResponse::Valid, json_result);
        }
    }

    Ok(())
}
