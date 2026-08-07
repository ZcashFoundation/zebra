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

/// The P2P sidecar zcashd follows Zebra's tip and peers with Zebra alone.
///
/// On managed (regtest) mode: mines 5 blocks via Zebra, waits for zcashd to
/// reach the same height over P2P, and asserts the best block hashes match.
/// On all modes: asserts zcashd has exactly one peer — the shield is
/// "connect only to Zebra, listen for nothing", so a second peer is a
/// misconfiguration even on live networks.
pub async fn sidecar_follows_tip() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if setup.can_mutate() {
        setup.zebra_client.generate(5).await?;
        wait_for_zcashd_height(&setup.zcashd_client, 5).await?;

        let zebra_best: String = setup
            .zebra_client
            .json_result_from_call("getbestblockhash", "[]")
            .await
            .map_err(|e| color_eyre::eyre::eyre!("zebrad getbestblockhash: {e}"))?;
        let zcashd_best: String = setup
            .zcashd_client
            .json_result_from_call("getbestblockhash", "[]")
            .await
            .map_err(|e| color_eyre::eyre::eyre!("zcashd getbestblockhash: {e}"))?;

        assert_eq!(
            zcashd_best, zebra_best,
            "zcashd should follow Zebra's best block over P2P"
        );
    }

    let peers: Vec<serde_json::Value> = setup
        .zcashd_client
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|e| color_eyre::eyre::eyre!("zcashd getpeerinfo: {e}"))?;

    assert_eq!(
        peers.len(),
        1,
        "the sidecar zcashd must peer with exactly one node (Zebra), got: {peers:?}"
    );
    assert_eq!(
        peers[0]["inbound"].as_bool(),
        Some(false),
        "the sidecar's single peer must be an outbound -connect to Zebra: {:?}",
        peers[0]
    );

    setup.teardown()
}

/// Miner-facing RPCs are removed from the sidecar zcashd build; Zebra is the
/// canonical block-template source.
///
/// Asserts `getblocktemplate`, `submitblock`, and `generate` return JSON-RPC
/// "Method not found" on zcashd, and (in managed mode) that Zebra's
/// `getblocktemplate` succeeds.
pub async fn miner_rpcs_disabled() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    for method in ["getblocktemplate", "submitblock", "generate"] {
        let error = setup
            .zcashd_client
            .json_result_from_call::<serde_json::Value>(method, "[]")
            .await
            .expect_err("miner RPCs must be unavailable on the sidecar zcashd");

        assert!(
            error.to_string().contains("-32601"),
            "zcashd {method} should fail with RPC_METHOD_NOT_FOUND (-32601), got: {error}"
        );
    }

    if setup.can_mutate() {
        // Mine one block first so Zebra has a non-genesis tip to build on.
        setup.zebra_client.generate(1).await?;

        let template: serde_json::Value = setup
            .zebra_client
            .json_result_from_call("getblocktemplate", "[]")
            .await
            .map_err(|e| color_eyre::eyre::eyre!("zebrad getblocktemplate: {e}"))?;

        assert!(
            template["height"].is_number(),
            "zebrad getblocktemplate should return a template: {template}"
        );
    }

    setup.teardown()
}
