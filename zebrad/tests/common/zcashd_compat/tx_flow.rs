//! Transaction flow test bodies for the zcashd-compat integration test suite.
//!
//! The sidecar zcashd build is shielded-first: the legacy transparent
//! `getnewaddress` is disabled, and transparent coinbase paid to a
//! unified-address receiver is not credited to the account. So these tests
//! drive the wallet through the account / unified-address / `z_*` flow, and
//! fund it by mining coinbase to the account's Sapling receiver via zebrad's
//! regtest-only `generatetoaddress`.

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tokio::time::sleep;

use super::{launch::ZcashdCompatSetup, setup_zcashd_compat, wait_for_zcashd_height};
use crate::common::regtest::MiningRpcMethods;

/// Blocks to mine before spending: coinbase matures at 100 confirmations on
/// regtest, so height 110 leaves the first ten coinbase notes spendable.
const FUNDING_BLOCKS: u64 = 110;

/// Creates a fresh wallet account on zcashd and returns its unified address
/// and Sapling receiver.
async fn new_account(setup: &ZcashdCompatSetup) -> Result<(String, String)> {
    let account: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("z_getnewaccount", "[]")
        .await
        .map_err(|e| eyre!("z_getnewaccount: {e}"))?;
    let account = account["account"]
        .as_u64()
        .ok_or_else(|| eyre!("missing `account` in z_getnewaccount response: {account}"))?;

    let response: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call(
            "z_getaddressforaccount",
            &format!(r#"[{account}, ["p2pkh", "sapling"]]"#),
        )
        .await
        .map_err(|e| eyre!("z_getaddressforaccount: {e}"))?;
    let unified_address = response["address"]
        .as_str()
        .ok_or_else(|| eyre!("missing `address` in z_getaddressforaccount response: {response}"))?
        .to_string();

    let receivers: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call(
            "z_listunifiedreceivers",
            &format!(r#"["{unified_address}"]"#),
        )
        .await
        .map_err(|e| eyre!("z_listunifiedreceivers: {e}"))?;
    let sapling_address = receivers["sapling"]
        .as_str()
        .ok_or_else(|| eyre!("missing `sapling` receiver in response: {receivers}"))?
        .to_string();

    Ok((unified_address, sapling_address))
}

/// Mines spendable coinbase to `sapling_address` on zebrad and waits for the
/// sidecar to sync it.
async fn fund_address(setup: &ZcashdCompatSetup, sapling_address: &str) -> Result<()> {
    let _: Vec<serde_json::Value> = setup
        .zebra_client
        .json_result_from_call(
            "generatetoaddress",
            &format!(r#"[{FUNDING_BLOCKS}, "{sapling_address}"]"#),
        )
        .await
        .map_err(|e| eyre!("generatetoaddress: {e}"))?;
    wait_for_zcashd_height(&setup.zcashd_client, FUNDING_BLOCKS).await
}

/// Sends a shielded transaction from `from_ua` to `to_ua` via `z_sendmany`,
/// polls the async operation to completion, and returns the txid.
async fn send_shielded(setup: &ZcashdCompatSetup, from_ua: &str, to_ua: &str) -> Result<String> {
    // Spending shielded coinbase to another account reveals the amount, which
    // needs an explicit privacy-policy opt-in.
    let opid: String = setup
        .zcashd_client
        .json_result_from_call(
            "z_sendmany",
            &format!(
                r#"["{from_ua}", [{{"address": "{to_ua}", "amount": 0.001}}], 1, null, "AllowRevealedAmounts"]"#
            ),
        )
        .await
        .map_err(|e| eyre!("z_sendmany: {e}"))?;

    // z_sendmany builds the shielded proof asynchronously; poll the operation.
    for _ in 0..60u32 {
        let status: serde_json::Value = setup
            .zcashd_client
            .json_result_from_call("z_getoperationstatus", &format!(r#"[["{opid}"]]"#))
            .await
            .map_err(|e| eyre!("z_getoperationstatus: {e}"))?;
        let status = &status[0];

        match status["status"].as_str() {
            Some("success") => {
                return status["result"]["txid"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| eyre!("missing `txid` in operation result: {status}"));
            }
            Some("failed") => return Err(eyre!("z_sendmany operation failed: {status}")),
            _ => sleep(Duration::from_secs(1)).await,
        }
    }

    Err(eyre!("z_sendmany operation did not complete within 60 s"))
}

/// Sends a shielded transaction via zcashd and confirms it appears in
/// zebrad's mempool.
///
/// In managed (regtest) mode: funds the wallet by mining coinbase to its
/// Sapling receiver, sends a shielded transaction, and polls zebrad's
/// `getrawmempool` until the txid appears.
///
/// In external mode: skips the send and instead validates the structural shape
/// of `getmempoolinfo` on zebrad.
pub async fn shielded_tx_in_mempool() -> Result<()> {
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

    let (from_ua, sapling_address) = new_account(&setup).await?;
    let (to_ua, _) = new_account(&setup).await?;
    fund_address(&setup, &sapling_address).await?;

    let txid = send_shielded(&setup, &from_ua, &to_ua).await?;

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

/// Sends a shielded transaction via zcashd, mines a block, and confirms the
/// transaction via zebrad's `getrawtransaction`.
///
/// Only runs in managed (regtest) mode; skipped on external networks.
pub async fn shielded_tx_confirms() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    let (from_ua, sapling_address) = new_account(&setup).await?;
    let (to_ua, _) = new_account(&setup).await?;
    fund_address(&setup, &sapling_address).await?;

    let txid = send_shielded(&setup, &from_ua, &to_ua).await?;

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
