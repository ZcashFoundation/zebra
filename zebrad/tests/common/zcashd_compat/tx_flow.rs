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
use tokio::time::{sleep, timeout};
use zebra_chain::{
    block::Height,
    parameters::{
        subsidy::{miner_subsidy, scheduled_block_subsidy},
        NetworkKind,
    },
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
    transparent,
};

use super::{
    config::read_test_network_kind, launch::ZcashdCompatSetup, setup_zcashd_compat,
    wait_for_zcashd_height, TEST_ZCASHD_COMPAT,
};
use crate::common::regtest::MiningRpcMethods;

/// Blocks used by the older long-chain funding cases, not the one-confirmation spend case.
const FUNDING_BLOCKS: u64 = 110;

/// Creates a fresh wallet account and returns its id, Unified Address, Sapling and p2pkh receivers.
async fn new_account(setup: &ZcashdCompatSetup) -> Result<(u64, String, String, String)> {
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
    let transparent_address = receivers["p2pkh"]
        .as_str()
        .ok_or_else(|| eyre!("missing `p2pkh` receiver in response: {receivers}"))?
        .to_string();

    Ok((
        account,
        unified_address,
        sapling_address,
        transparent_address,
    ))
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

    wait_for_send_operation(setup, &opid).await
}

/// Waits for the wallet proof operation to succeed and returns its broadcast transaction id.
async fn wait_for_send_operation(setup: &ZcashdCompatSetup, opid: &str) -> Result<String> {
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

    let (_, from_ua, sapling_address, _) = new_account(&setup).await?;
    let (_, to_ua, _, _) = new_account(&setup).await?;
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

    let (_, from_ua, sapling_address, _) = new_account(&setup).await?;
    let (_, to_ua, _, _) = new_account(&setup).await?;
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

/// Ports issue #9690's immediate Sapling coinbase spend with shielded and transparent recipients.
///
/// Requires an explicitly enabled, fresh managed Regtest sidecar. Unlike the historical upstream
/// test, all upgrades through NU5 are active at height one; Heartwood rejection is not covered.
/// The original sidecar conventional fee for three actions is explicitly 15,000 zatoshis.
pub async fn shielded_coinbase_spends_at_one_confirmation() -> Result<()> {
    if std::env::var(TEST_ZCASHD_COMPAT).as_deref() != Ok("1") {
        return Err(eyre!(
            "this case requires {TEST_ZCASHD_COMPAT}=1; a skip is not coverage"
        ));
    }
    if read_test_network_kind()? != NetworkKind::Regtest {
        return Err(eyre!(
            "this case requires managed Regtest, not an external wallet"
        ));
    }

    // Reserve the remaining 30 seconds of the 180-second budget for synchronous teardown.
    timeout(Duration::from_secs(150), async {
        let setup = timeout(Duration::from_secs(45), setup_zcashd_compat())
            .await
            .map_err(|_| eyre!("managed sidecar startup exceeded 45 s; prefetch its binary"))??
            .ok_or_else(|| eyre!("managed sidecar setup unexpectedly skipped"))?;
        assert!(setup.managed.is_some() && setup.can_mutate());

        let (sender, from_ua, funding_sapling, _) =
            timeout(Duration::from_secs(15), new_account(&setup)).await??;
        let (receiver, _, recipient_sapling, recipient_transparent) =
            timeout(Duration::from_secs(15), new_account(&setup)).await??;
        assert_ne!(sender, receiver);

        let height: u64 = timeout(
            Duration::from_secs(15),
            setup
                .zebra_client
                .json_result_from_call("getblockcount", "[]"),
        )
        .await?
        .map_err(|e| eyre!("initial getblockcount: {e}"))?;
        assert_eq!(height, 0, "fund exactly one coinbase on a fresh chain");
        let reward = u64::from(miner_subsidy(
            Height(1),
            &setup.network,
            scheduled_block_subsidy(Height(1), &setup.network)?,
        )?);
        const RECIPIENT_ZAT: u64 = 200_000_000;
        // conventional_fee(3) in the pinned sidecar and original test: 5,000 * max(2, 3).
        // Zebra's newer marginal fee differs; do not silently change the wallet's explicit fee.
        const FEE_ZAT: u64 = 5_000 * 3;
        let expected_change = reward
            .checked_sub(2 * RECIPIENT_ZAT + FEE_ZAT)
            .ok_or_else(|| eyre!("miner reward must cover both recipients and the explicit fee"))?;

        let funded: Vec<String> = timeout(
            Duration::from_secs(30),
            setup.zebra_client.json_result_from_call(
                "generatetoaddress",
                &format!(r#"[1, "{funding_sapling}"]"#),
            ),
        )
        .await?
        .map_err(|e| eyre!("one Sapling coinbase: {e}"))?;
        assert_eq!(funded.len(), 1);
        timeout(
            Duration::from_secs(15),
            wait_for_zcashd_height(&setup.zcashd_client, 1),
        )
        .await??;
        wait_for_sapling_balance(&setup, sender, reward).await?;
        let sidecar_height: u64 = timeout(
            Duration::from_secs(15),
            setup
                .zcashd_client
                .json_result_from_call("getblockcount", "[]"),
        )
        .await?
        .map_err(|e| eyre!("funded sidecar height: {e}"))?;
        assert_eq!(
            sidecar_height, 1,
            "spend at one confirmation, without a maturity wait"
        );

        let opid: String = timeout(
            Duration::from_secs(15),
            setup.zcashd_client.json_result_from_call(
                "z_sendmany",
                &format!(
                    r#"["{from_ua}", [
                        {{"address": "{recipient_sapling}", "amount": 2}},
                        {{"address": "{recipient_transparent}", "amount": 2}}
                    ], 1, 0.00015000, "AllowRevealedRecipients"]"#
                ),
            ),
        )
        .await?
        .map_err(|e| eyre!("one-confirmation mixed z_sendmany: {e}"))?;
        let txid = timeout(
            Duration::from_secs(45),
            wait_for_send_operation(&setup, &opid),
        )
        .await
        .map_err(|_| eyre!("mixed spend operation {opid} exceeded 45 s"))??;
        timeout(
            Duration::from_secs(20),
            wait_for_zebra_mempool_tx(&setup, &txid),
        )
        .await
        .map_err(|_| eyre!("mixed spend {txid} did not reach Zebra's mempool within 20 s"))??;

        // The confirmation block pays the harness's unrelated transparent miner, not either account.
        let blocks = timeout(Duration::from_secs(15), setup.zebra_client.generate(1)).await??;
        assert_eq!(blocks.len(), 1);
        let tx_info: serde_json::Value = timeout(
            Duration::from_secs(15),
            setup
                .zebra_client
                .json_result_from_call("getrawtransaction", &format!(r#"["{txid}", 1]"#)),
        )
        .await?
        .map_err(|e| eyre!("confirmed mixed getrawtransaction: {e}"))?;
        assert_eq!(
            tx_info["confirmations"], 1,
            "mixed spend must be confirmed exactly once"
        );
        assert_eq!(tx_info["blockhash"], blocks[0].to_string());
        let raw = tx_info["hex"]
            .as_str()
            .ok_or_else(|| eyre!("confirmed getrawtransaction missing hex: {tx_info}"))?;
        let tx: Transaction = hex::decode(raw)?.as_slice().zcash_deserialize_into()?;
        assert_eq!(tx.hash().to_string(), txid);
        assert!(!tx.is_coinbase());
        assert!(
            tx.orchard_bundle().is_none(),
            "the recipients and source are Sapling/p2pkh"
        );
        let recipient: transparent::Address = recipient_transparent.parse()?;
        let outputs = tx.outputs();
        assert_eq!(
            outputs.len(),
            1,
            "mixed spend has exactly one transparent recipient"
        );
        assert_eq!(outputs[0].address(&setup.network), Some(recipient));
        assert_eq!(u64::from(outputs[0].value), RECIPIENT_ZAT);

        timeout(
            Duration::from_secs(15),
            wait_for_zcashd_height(&setup.zcashd_client, 2),
        )
        .await??;
        wait_for_sapling_balance(&setup, receiver, RECIPIENT_ZAT).await?;
        wait_for_sapling_balance(&setup, sender, expected_change).await?;
        setup.teardown()
    })
    .await
    .map_err(|_| eyre!("mixed coinbase spend exceeded 150 s (180 s including cleanup)"))?
}

/// Requires the account's confirmed Sapling pool balance, allowing a short wallet-scan delay.
async fn wait_for_sapling_balance(
    setup: &ZcashdCompatSetup,
    account: u64,
    expected_zat: u64,
) -> Result<()> {
    timeout(Duration::from_secs(15), async {
        loop {
            let balance: serde_json::Value = setup
                .zcashd_client
                .json_result_from_call("z_getbalanceforaccount", &format!("[{account}, 1]"))
                .await
                .map_err(|e| eyre!("z_getbalanceforaccount({account}, 1): {e}"))?;
            assert_eq!(balance["minimum_confirmations"], 1);
            let pools = balance["pools"]
                .as_object()
                .ok_or_else(|| eyre!("account balance missing pools: {balance}"))?;
            if let Some(sapling) = pools.get("sapling") {
                let value = sapling["valueZat"]
                    .as_u64()
                    .ok_or_else(|| eyre!("invalid Sapling pool value: {balance}"))?;
                if value == expected_zat {
                    return Ok(());
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        eyre!("account {account} Sapling minconf=1 balance did not reach {expected_zat}")
    })?
}
