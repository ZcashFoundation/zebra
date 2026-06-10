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

/// Verifies that zcashd can generate a new transparent address.
///
/// Calls `getnewaddress` on zcashd and asserts the returned value is a
/// non-empty string.  Works on all networks.
pub async fn address_generation() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    exit_initial_block_download(&setup).await?;

    let address: String = setup
        .zcashd_client
        .json_result_from_call("getnewaddress", "[]")
        .await
        .map_err(|e| eyre!("getnewaddress: {e}"))?;

    assert!(
        !address.is_empty(),
        "getnewaddress returned an empty string"
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
