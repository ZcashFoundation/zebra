//! Chain-state consistency test bodies for the zcashd-compat integration test suite.

use color_eyre::eyre::{eyre, Result};

use super::{setup_zcashd_compat, wait_for_zcashd_height};
use crate::common::regtest::MiningRpcMethods;

/// Verifies that zebrad and zcashd agree on block count and best block hash.
///
/// In managed (regtest) mode, mines 5 blocks first and asserts the count is
/// exactly 5.  In external mode, skips mining and only cross-checks that both
/// sides report the same height and hash.
pub async fn height_and_hash_agree() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if setup.can_mutate() {
        setup.zebra_client.generate(5).await?;
        wait_for_zcashd_height(&setup.zcashd_client, 5).await?;
    }

    let zebra_count: u64 = setup
        .zebra_client
        .json_result_from_call("getblockcount", "[]")
        .await
        .map_err(|e| eyre!("zebrad getblockcount: {e}"))?;

    let zcashd_count: u64 = setup
        .zcashd_client
        .json_result_from_call("getblockcount", "[]")
        .await
        .map_err(|e| eyre!("zcashd getblockcount: {e}"))?;

    if setup.can_mutate() {
        assert_eq!(
            zebra_count, 5,
            "zebrad should have exactly 5 blocks after mining"
        );
    }

    assert_eq!(
        zebra_count, zcashd_count,
        "zebrad and zcashd disagree on block count: {zebra_count} vs {zcashd_count}"
    );

    let zebra_hash: serde_json::Value = setup
        .zebra_client
        .json_result_from_call("getbestblockhash", "[]")
        .await
        .map_err(|e| eyre!("zebrad getbestblockhash: {e}"))?;

    let zcashd_hash: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("getbestblockhash", "[]")
        .await
        .map_err(|e| eyre!("zcashd getbestblockhash: {e}"))?;

    assert_eq!(
        zebra_hash, zcashd_hash,
        "zebrad and zcashd disagree on best block hash"
    );

    setup.teardown()
}

/// Verifies that `getblockhash` returns the same result on both endpoints for a
/// range of historical heights.
///
/// In managed (regtest) mode, mines 3 blocks and checks heights 1–3.
/// In external mode, queries the current tip and checks the last 3 block hashes.
pub async fn getblock_hash_consistent() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    let start_height: u64 = if setup.can_mutate() {
        setup.zebra_client.generate(3).await?;
        wait_for_zcashd_height(&setup.zcashd_client, 3).await?;
        1
    } else {
        let tip: u64 = setup
            .zebra_client
            .json_result_from_call("getblockcount", "[]")
            .await
            .map_err(|e| eyre!("zebrad getblockcount: {e}"))?;
        tip.saturating_sub(2)
    };

    for height in start_height..=start_height + 2 {
        let params = format!("[{height}]");

        let zebra_hash: serde_json::Value = setup
            .zebra_client
            .json_result_from_call("getblockhash", &params)
            .await
            .map_err(|e| eyre!("zebrad getblockhash({height}): {e}"))?;

        let zcashd_hash: serde_json::Value = setup
            .zcashd_client
            .json_result_from_call("getblockhash", &params)
            .await
            .map_err(|e| eyre!("zcashd getblockhash({height}): {e}"))?;

        assert_eq!(
            zebra_hash, zcashd_hash,
            "block hash mismatch at height {height}: zebrad={zebra_hash} zcashd={zcashd_hash}"
        );
    }

    setup.teardown()
}
