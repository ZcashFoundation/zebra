//! Wallet RPC test bodies for the zcashd-compat integration test suite.

use color_eyre::eyre::{eyre, Result};

use super::setup_zcashd_compat;

/// Verifies that zcashd can generate a new transparent address.
///
/// Calls `getnewaddress` on zcashd and asserts the returned value is a
/// non-empty string.  Works on all networks.
pub async fn address_generation() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    let address: String = setup
        .zcashd_client
        .json_result_from_call("getnewaddress", "[]")
        .await
        .map_err(|e| eyre!("getnewaddress: {e}"))?;

    assert!(!address.is_empty(), "getnewaddress returned an empty string");

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

    let balance: f64 = setup
        .zcashd_client
        .json_result_from_call("getbalance", "[]")
        .await
        .map_err(|e| eyre!("getbalance: {e}"))?;

    assert_eq!(balance, 0.0, "expected zero balance on fresh regtest wallet");

    setup.teardown()
}

/// Verifies that `getwalletinfo` contains the expected response fields.
///
/// Works on all networks.
pub async fn getwalletinfo_fields_present() -> Result<()> {
    let Some(setup) = setup_zcashd_compat().await? else {
        return Ok(());
    };

    let info: serde_json::Value = setup
        .zcashd_client
        .json_result_from_call("getwalletinfo", "[]")
        .await
        .map_err(|e| eyre!("getwalletinfo: {e}"))?;

    for field in &["walletversion", "balance", "unconfirmed_balance", "immature_balance"] {
        assert!(
            info.get(field).is_some(),
            "getwalletinfo response missing field `{field}`: {info}"
        );
    }

    setup.teardown()
}
