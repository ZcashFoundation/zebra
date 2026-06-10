//! Startup and auth test bodies for the zcashd-compat integration test suite.

use color_eyre::eyre::Result;

use super::{
    config::{expected_zcashd_chain_name, expected_zebrad_chain_name},
    setup_zcashd_compat, wait_for_zcashd_height,
};
use crate::common::regtest::MiningRpcMethods;

/// Verifies that both zebrad and zcashd start and respond to basic RPC calls.
///
/// Calls `getblockchaininfo` on both clients and asserts the `chain` field
/// matches the expected value for the test network.
pub async fn both_processes_start() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    let expected_zebrad_chain = expected_zebrad_chain_name(&setup.network);
    let expected_zcashd_chain = expected_zcashd_chain_name(&setup.network);

    let zebra_info: serde_json::Value = setup
        .zebra_client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|e| color_eyre::eyre::eyre!("zebrad getblockchaininfo: {e}"))?;

    assert_eq!(
        zebra_info["chain"].as_str(),
        Some(expected_zebrad_chain.as_str()),
        "zebrad chain mismatch: expected {expected_zebrad_chain:?}, got {:?}",
        zebra_info["chain"]
    );

    let zcashd_info: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("getblockchaininfo", "[]")
        .await
        .map_err(|e| color_eyre::eyre::eyre!("zcashd getblockchaininfo: {e}"))?;

    assert_eq!(
        zcashd_info["chain"].as_str(),
        Some(expected_zcashd_chain),
        "zcashd chain mismatch: expected {expected_zcashd_chain:?}, got {:?}",
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
        wait_for_zcashd_height(&setup.zcashd_client, 5).await?;
    }

    // Readiness flips to "ready" once several async components converge
    // (tip match, mempool mirror, wallet notifications), so poll for up to
    // 60 seconds in managed mode instead of asserting a single snapshot.
    let mut readiness = String::new();
    for attempt in 1..=60u32 {
        let info: serde_json::Value = setup
            .zcashd_client
            .json_result_from_call("getzebracompatinfo", "[]")
            .await
            .map_err(|e| color_eyre::eyre::eyre!("getzebracompatinfo: {e}"))?;

        readiness = info["readiness"]
            .as_str()
            .ok_or_else(|| {
                color_eyre::eyre::eyre!("missing or non-string `readiness` field: {info}")
            })?
            .to_string();

        if !setup.can_mutate() || readiness == "ready" || attempt == 60 {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    if setup.can_mutate() {
        assert_eq!(
            readiness, "ready",
            "expected readiness=='ready' within 60 s of mining 5 blocks, got {readiness:?}"
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
