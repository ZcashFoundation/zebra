//! Transaction flow test bodies for the zcashd-compat integration test suite.

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tokio::time::sleep;

use super::{
    config::MINER_PRIV_WIF, launch::ZcashdCompatSetup, setup_zcashd_compat, wait_for_zcashd_height,
};
use crate::common::regtest::MiningRpcMethods;

/// Imports the deterministic miner private key into zcashd's wallet (with a
/// rescan), making the mined coinbase outputs spendable via `sendtoaddress`.
async fn import_miner_key(setup: &ZcashdCompatSetup) -> Result<()> {
    let _: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call(
            "importprivkey",
            &format!(r#"["{MINER_PRIV_WIF}", "", true]"#),
        )
        .await
        .map_err(|e| eyre!("importprivkey: {e}"))?;
    Ok(())
}

/// Sends a transparent transaction via zcashd and confirms it appears in
/// zebrad's mempool.
///
/// In managed (regtest) mode: funds the wallet by mining coinbase, sends a
/// transaction, and polls zebrad's `getrawmempool` until the txid appears.
///
/// In external mode: skips the send and instead validates the structural shape
/// of `getmempoolinfo` on zebrad.
pub async fn transparent_tx_in_mempool() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        // On live networks, just check that getmempoolinfo has the expected fields.
        let info: serde_json::Value = setup
            .zebra_client
            .json_result_from_call("getmempoolinfo", "[]")
            .await
            .map_err(|e| eyre!("getmempoolinfo: {e}"))?;

        for field in &["size", "bytes"] {
            assert!(
                info.get(field).is_some(),
                "getmempoolinfo missing field `{field}`: {info}"
            );
        }
        return setup.teardown();
    }

    // Mine enough blocks to have spendable coinbase (maturity = 100 on regtest).
    setup.zebra_client.generate(110).await?;
    wait_for_zcashd_height(&setup.zcashd_client, 110).await?;
    import_miner_key(&setup).await?;

    // Get a fresh address and send some ZEC to it.
    let addr: String = setup
        .zcashd_client
        .json_result_from_call("getnewaddress", "[]")
        .await
        .map_err(|e| eyre!("getnewaddress: {e}"))?;

    let txid: String = setup
        .zcashd_client
        .json_result_from_call("sendtoaddress", &format!(r#"["{addr}", 0.001]"#))
        .await
        .map_err(|e| eyre!("sendtoaddress: {e}"))?;

    wait_for_zebra_mempool_tx(&setup, &txid).await?;

    setup.teardown()
}

/// Polls zebrad's `getrawmempool` until `txid` appears (up to 30 s).
async fn wait_for_zebra_mempool_tx(setup: &ZcashdCompatSetup, txid: &str) -> Result<()> {
    for attempt in 1..=30u32 {
        let mempool: Vec<String> = setup
            .zebra_client
            .json_result_from_call("getrawmempool", "[]")
            .await
            .map_err(|e| eyre!("getrawmempool: {e}"))?;

        if mempool.iter().any(|entry| entry == txid) {
            return Ok(());
        }

        if attempt == 30 {
            return Err(eyre!(
                "txid {txid} never appeared in zebrad mempool after 30 s"
            ));
        }
        sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

/// Sends a transparent transaction via zcashd, mines a block, and confirms the
/// transaction via zebrad's `getrawtransaction`.
///
/// Only runs in managed (regtest) mode; skipped on external networks.
pub async fn transparent_tx_confirms() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    // Mine enough blocks to have spendable coinbase.
    setup.zebra_client.generate(110).await?;
    wait_for_zcashd_height(&setup.zcashd_client, 110).await?;
    import_miner_key(&setup).await?;

    let addr: String = setup
        .zcashd_client
        .json_result_from_call("getnewaddress", "[]")
        .await
        .map_err(|e| eyre!("getnewaddress: {e}"))?;

    let txid: String = setup
        .zcashd_client
        .json_result_from_call("sendtoaddress", &format!(r#"["{addr}", 0.001]"#))
        .await
        .map_err(|e| eyre!("sendtoaddress: {e}"))?;

    // Wait for the transaction to relay from zcashd to zebrad over P2P before
    // mining: zcashd trickles tx invs to peers, so mining immediately would
    // build a block that misses the transaction.
    wait_for_zebra_mempool_tx(&setup, &txid).await?;

    // Mine a block to confirm the transaction.
    setup.zebra_client.generate(1).await?;

    // Verify via zebrad that the transaction has at least one confirmation.
    let tx_info: serde_json::Value = setup
        .zebra_client
        .json_result_from_call("getrawtransaction", &format!(r#"["{txid}", 1]"#))
        .await
        .map_err(|e| eyre!("getrawtransaction: {e}"))?;

    let confirmations = tx_info["confirmations"]
        .as_u64()
        .ok_or_else(|| eyre!("missing `confirmations` in getrawtransaction response: {tx_info}"))?;

    assert!(
        confirmations >= 1,
        "expected at least 1 confirmation, got {confirmations}"
    );

    setup.teardown()
}
