//! Startup and auth test bodies for the zcashd-compat integration test suite.

use color_eyre::eyre::Result;

use crate::common::regtest::MiningRpcMethods;
use super::{config::expected_chain_name, setup_zcashd_compat};

/// Verifies that both zebrad and zcashd start and respond to basic RPC calls.
///
/// Calls `getblockchaininfo` on both clients and asserts the `chain` field
/// matches the expected value for the test network.
pub async fn both_processes_start() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    let expected_chain = expected_chain_name(&setup.network);

    let zebra_info: serde_json::Value = setup
        .zebra_client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|e| color_eyre::eyre::eyre!("zebrad getblockchaininfo: {e}"))?;

    assert_eq!(
        zebra_info["chain"].as_str(),
        Some(expected_chain),
        "zebrad chain mismatch: expected {expected_chain:?}, got {:?}",
        zebra_info["chain"]
    );

    let zcashd_info: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|e| color_eyre::eyre::eyre!("zcashd getblockchaininfo: {e}"))?;

    assert_eq!(
        zcashd_info["chain"].as_str(),
        Some(expected_chain),
        "zcashd chain mismatch: expected {expected_chain:?}, got {:?}",
        zcashd_info["chain"]
    );

    setup.teardown()
}

/// After mining 5 blocks, `getzebracompatinfo` on zcashd should report readiness.
///
/// On managed (regtest) mode: mines 5 blocks, then asserts `readiness == "ready"`.
/// On external mode: calls `getzebracompatinfo` without mining and checks the field
/// is a non-null string (live chain may still be syncing).
pub async fn readiness_after_mine() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if setup.can_mutate() {
        setup.zebra_client.generate(5).await?;
    }

    let info: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("getzebracompatinfo", "[]")
        .await
        .map_err(|e| color_eyre::eyre::eyre!("getzebracompatinfo: {e}"))?;

    let readiness = info["readiness"]
        .as_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing or non-string `readiness` field: {info}"))?;

    if setup.can_mutate() {
        assert_eq!(
            readiness, "ready",
            "expected readiness=='ready' after mining 5 blocks, got {readiness:?}"
        );
    }

    setup.teardown()
}

/// Verifies that the zebrad zcashd-compat RPC endpoint rejects requests without
/// valid credentials.
///
/// This test only runs in managed (regtest) mode where we own the zcashd-compat
/// RPC endpoint and know its authentication cookie.
pub async fn rpc_requires_auth() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        // External mode: we don't control the zcashd-compat RPC configuration.
        return setup.teardown();
    }

    // Make a raw unauthenticated POST to the zcashd-compat endpoint.
    let response = reqwest::Client::new()
        .post(format!("http://{}", setup.zebra_compat_rpc_addr))
        .header("Content-Type", "application/json")
        .body(r#"{"jsonrpc":"2.0","method":"getblockchaininfo","params":[],"id":1}"#)
        .send()
        .await;

    match response {
        Err(_) => {
            // Connection rejected — authentication is working.
        }
        Ok(resp) => {
            let text = resp.text().await.unwrap_or_default();
            assert!(
                !text.contains(r#""result""#)
                    || text.contains(r#""error""#)
                    || text.is_empty(),
                "unauthenticated request should not return a successful JSON-RPC result, got: {text}"
            );
        }
    }

    setup.teardown()
}
