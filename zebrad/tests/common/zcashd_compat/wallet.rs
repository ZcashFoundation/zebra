//! Wallet RPC test bodies for the zcashd-compat integration test suite.

use color_eyre::eyre::{eyre, Result};

use super::{launch::ZcashdCompatSetup, setup_zcashd_compat, wait_for_zcashd_height};
use crate::common::regtest::MiningRpcMethods;

/// Mines one block and waits for zcashd to sync it (managed mode only).
///
/// zcashd disables wallet RPCs while in initial block download, which on a
/// fresh regtest chain only clears once the first block arrives.
async fn exit_initial_block_download(setup: &ZcashdCompatSetup) -> Result<()> {
    if setup.can_mutate() {
        setup.zebra_client.generate(1).await?;
        wait_for_zcashd_height(&setup.zcashd_client, 1).await?;
    }
    Ok(())
}

/// Verifies that zcashd can create a wallet account and derive a unified
/// address for it.
///
/// The sidecar zcashd build is shielded-first and disables the legacy
/// transparent `getnewaddress`, so address generation goes through the
/// account / unified-address flow.  Works on all networks.
pub async fn address_generation() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    exit_initial_block_download(&setup).await?;

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
    let address = response["address"]
        .as_str()
        .ok_or_else(|| eyre!("missing `address` in z_getaddressforaccount response: {response}"))?;

    assert!(
        !address.is_empty(),
        "z_getaddressforaccount returned an empty address"
    );

    setup.teardown()
}

/// Verifies that the zcashd wallet balance is zero before any funding.
///
/// Only meaningful in managed (regtest) mode where the wallet starts fresh.
/// Skipped on external mode because the wallet may already hold funds.
pub async fn initial_balance_zero() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    if !setup.can_mutate() {
        return setup.teardown();
    }

    // Mines to zebrad's miner address, so the zcashd wallet balance stays zero.
    exit_initial_block_download(&setup).await?;

    let balance: f64 = setup
        .zcashd_client
        .json_result_from_call("getbalance", "[]")
        .await
        .map_err(|e| eyre!("getbalance: {e}"))?;

    assert_eq!(
        balance, 0.0,
        "expected zero balance on fresh regtest wallet"
    );

    setup.teardown()
}

/// Verifies that `getwalletinfo` contains the expected response fields.
///
/// Works on all networks.
pub async fn getwalletinfo_fields_present() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    exit_initial_block_download(&setup).await?;

    let info: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("getwalletinfo", "[]")
        .await
        .map_err(|e| eyre!("getwalletinfo: {e}"))?;

    for field in &[
        "walletversion",
        "balance",
        "unconfirmed_balance",
        "immature_balance",
    ] {
        assert!(
            info.get(field).is_some(),
            "getwalletinfo response missing field `{field}`: {info}"
        );
    }

    setup.teardown()
}
