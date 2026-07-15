//! Live-network validation test bodies for the zcashd-compat integration test suite.
//!
//! These tests are primarily useful in external (mainnet/testnet) mode where a
//! real network is present.  They are skipped or reduced to structural checks
//! in managed (regtest) mode.

use color_eyre::eyre::{eyre, Result};

use super::setup_zcashd_compat;

/// Verifies that zebrad is connected to at least one peer on the network.
///
/// Calls `getpeerinfo` on zebrad and asserts a non-empty peer list.
///
/// Skipped in managed (regtest) mode — regtest nodes do not connect to external
/// peers.
pub async fn peer_connectivity() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if setup.can_mutate() {
        return setup.teardown();
    }

    let peers: Vec<serde_json::Value> = setup
        .zebra_client
        .json_result_from_call("getpeerinfo", "[]")
        .await
        .map_err(|e| eyre!("getpeerinfo: {e}"))?;

    assert!(
        !peers.is_empty(),
        "zebrad has no connected peers on {network}; is it actually synced?",
        network = setup.network
    );

    setup.teardown()
}

/// Verifies that `getmempoolinfo` returns a structurally valid response.
///
/// Works on all networks.  On live networks the mempool will usually be
/// non-empty; on regtest it will be empty but the response shape is still
/// validated.
pub async fn mempool_info_valid() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    let info: serde_json::Value = setup
        .zebra_client
        .json_result_from_call("getmempoolinfo", "[]")
        .await
        .map_err(|e| eyre!("getmempoolinfo: {e}"))?;

    for field in &["size", "bytes", "usage"] {
        let value = info
            .get(field)
            .ok_or_else(|| eyre!("getmempoolinfo missing field `{field}`: {info}"))?;
        assert!(
            value.is_number(),
            "getmempoolinfo field `{field}` should be a number, got {value}"
        );
        let n = value.as_f64().unwrap_or(-1.0);
        assert!(
            n >= 0.0,
            "getmempoolinfo field `{field}` should be non-negative, got {n}"
        );
    }

    setup.teardown()
}

/// Verifies that zebrad and zcashd agree on the hash of the block at height 1.
///
/// A well-known historical block provides a chain-ID check without any write
/// operations.  Skipped in managed (regtest) mode because there is no pre-existing
/// history — the chain starts empty and we do not mine in this test.
pub async fn historical_block_consistent() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if setup.can_mutate() {
        return setup.teardown();
    }

    let params = "[1]";

    let zebra_hash: serde_json::Value = setup
        .zebra_client
        .json_result_from_call("getblockhash", params)
        .await
        .map_err(|e| eyre!("zebrad getblockhash(1): {e}"))?;

    let zcashd_hash: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("getblockhash", params)
        .await
        .map_err(|e| eyre!("zcashd getblockhash(1): {e}"))?;

    assert_eq!(
        zebra_hash, zcashd_hash,
        "zebrad and zcashd disagree on block hash at height 1"
    );

    setup.teardown()
}
