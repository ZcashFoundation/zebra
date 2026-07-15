//! zcashd-compat integration tests.
//!
//! These tests launch zebrad in zcashd-compat mode with a supervised zcashd
//! sidecar on Regtest, and verify startup, chain agreement, wallet RPCs,
//! transaction flow, resilience, and reorg handling across the pair.
//!
//! They require a sidecar zcashd binary and are skipped unless
//! `TEST_ZCASHD_COMPAT=1` is set. Run the full suite with:
//!
//! ```console
//! TEST_ZCASHD_COMPAT=1 cargo nextest run --profile zcashd-compat-integration --run-ignored=only
//! ```
//!
//! Or with an explicit binary:
//!
//! ```console
//! TEST_ZCASHD_COMPAT=1 TEST_ZCASHD_PATH=/path/to/zcashd \
//!   cargo nextest run --profile zcashd-compat-integration --run-ignored=only
//! ```

use color_eyre::eyre::Result;

use crate::common;

/// Verifies that both zebrad and zcashd start and respond to basic RPC calls.
///
/// See [`common::zcashd_compat::startup::both_processes_start`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_both_processes_start() -> Result<()> {
    common::zcashd_compat::startup::both_processes_start().await
}

/// The P2P sidecar zcashd follows Zebra's mined tip and peers with Zebra alone.
///
/// See [`common::zcashd_compat::startup::sidecar_follows_tip`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_sidecar_follows_tip() -> Result<()> {
    common::zcashd_compat::startup::sidecar_follows_tip().await
}

/// Miner-facing RPCs are removed from the sidecar zcashd; Zebra serves templates.
///
/// See [`common::zcashd_compat::startup::miner_rpcs_disabled`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_miner_rpcs_disabled() -> Result<()> {
    common::zcashd_compat::startup::miner_rpcs_disabled().await
}

/// Verifies that zebrad and zcashd agree on block count and best block hash.
///
/// See [`common::zcashd_compat::chain::height_and_hash_agree`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_height_and_hash_agree() -> Result<()> {
    common::zcashd_compat::chain::height_and_hash_agree().await
}

/// Verifies that `getblockhash` returns the same hash on both endpoints for heights 1–3.
///
/// See [`common::zcashd_compat::chain::getblock_hash_consistent`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_getblock_hash_consistent() -> Result<()> {
    common::zcashd_compat::chain::getblock_hash_consistent().await
}

/// Verifies that zcashd can generate new transparent and shielded addresses.
///
/// See [`common::zcashd_compat::wallet::address_generation`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_wallet_address_generation() -> Result<()> {
    common::zcashd_compat::wallet::address_generation().await
}

/// Verifies that wallet balances and UTXOs are zero before any funding.
///
/// See [`common::zcashd_compat::wallet::initial_balance_zero`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_wallet_initial_balance_zero() -> Result<()> {
    common::zcashd_compat::wallet::initial_balance_zero().await
}

/// Verifies that `getwalletinfo` contains all expected response fields.
///
/// See [`common::zcashd_compat::wallet::getwalletinfo_fields_present`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_getwalletinfo_fields_present() -> Result<()> {
    common::zcashd_compat::wallet::getwalletinfo_fields_present().await
}

/// Sends a shielded transaction via zcashd and confirms it appears in zebrad's mempool.
///
/// See [`common::zcashd_compat::tx_flow::shielded_tx_in_mempool`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_shielded_tx_in_mempool() -> Result<()> {
    common::zcashd_compat::tx_flow::shielded_tx_in_mempool().await
}

/// Sends a shielded transaction via zcashd, mines a block, and confirms it via zebrad.
///
/// See [`common::zcashd_compat::tx_flow::shielded_tx_confirms`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_shielded_tx_confirms() -> Result<()> {
    common::zcashd_compat::tx_flow::shielded_tx_confirms().await
}

/// Verifies that an abruptly SIGKILLed zebrad exits while supervising a running zcashd.
///
/// See [`common::zcashd_compat::resilience::zebrad_abrupt_kill`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_zebrad_abrupt_kill() -> Result<()> {
    common::zcashd_compat::resilience::zebrad_abrupt_kill().await
}

/// Verifies that zebrad's graceful SIGTERM shutdown also stops the supervised zcashd.
///
/// See [`common::zcashd_compat::resilience::zebrad_graceful_shutdown_stops_zcashd`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_zebrad_graceful_shutdown_stops_zcashd() -> Result<()> {
    common::zcashd_compat::resilience::zebrad_graceful_shutdown_stops_zcashd().await
}

/// Verifies that zcashd restarts after an unexpected exit without corrupting zebrad.
///
/// See [`common::zcashd_compat::resilience::zcashd_restarts_after_exit`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_zcashd_restarts_after_exit() -> Result<()> {
    common::zcashd_compat::resilience::zcashd_restarts_after_exit().await
}

/// See [`common::zcashd_compat::network::peer_connectivity`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_peer_connectivity() -> Result<()> {
    common::zcashd_compat::network::peer_connectivity().await
}

/// See [`common::zcashd_compat::network::mempool_info_valid`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_mempool_info_valid() -> Result<()> {
    common::zcashd_compat::network::mempool_info_valid().await
}

/// See [`common::zcashd_compat::network::historical_block_consistent`] for details.
#[tokio::test]
#[ignore]
async fn zcashd_compat_historical_block_consistent() -> Result<()> {
    common::zcashd_compat::network::historical_block_consistent().await
}

/// See [`common::zcashd_compat::reorg::basic_depth1`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_basic_depth1() -> Result<()> {
    common::zcashd_compat::reorg::basic_depth1().await
}

/// See [`common::zcashd_compat::reorg::equal_work_race`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_equal_work_race() -> Result<()> {
    common::zcashd_compat::reorg::equal_work_race().await
}

/// See [`common::zcashd_compat::reorg::deep_reorg_depth33`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_deep_depth33() -> Result<()> {
    common::zcashd_compat::reorg::deep_reorg_depth33().await
}

/// See [`common::zcashd_compat::reorg::deep_reorg_depth80`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_deep_depth80() -> Result<()> {
    common::zcashd_compat::reorg::deep_reorg_depth80().await
}

/// See [`common::zcashd_compat::reorg::deep_reorg_restart_recovers`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_deep_restart_recovers() -> Result<()> {
    common::zcashd_compat::reorg::deep_reorg_restart_recovers().await
}

/// See [`common::zcashd_compat::reorg::restart_after_reorg`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_restart_after_reorg() -> Result<()> {
    common::zcashd_compat::reorg::restart_after_reorg().await
}

/// See [`common::zcashd_compat::reorg::restart_cycles`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_restart_cycles() -> Result<()> {
    common::zcashd_compat::reorg::restart_cycles().await
}

/// See [`common::zcashd_compat::reorg::restart_deep_chain`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_restart_deep_chain() -> Result<()> {
    common::zcashd_compat::reorg::restart_deep_chain().await
}

/// See [`common::zcashd_compat::reorg::zebra_tip_behind_local`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_zebra_tip_behind_local() -> Result<()> {
    common::zcashd_compat::reorg::zebra_tip_behind_local().await
}

/// See [`common::zcashd_compat::reorg::reorg_context_zebra_tip_behind_recovers`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_context_zebra_tip_behind_recovers() -> Result<()> {
    common::zcashd_compat::reorg::reorg_context_zebra_tip_behind_recovers().await
}

/// See [`common::zcashd_compat::reorg::churn`] for details.
#[tokio::test]
#[ignore]
#[cfg(unix)]
async fn zcashd_compat_reorg_churn() -> Result<()> {
    common::zcashd_compat::reorg::churn().await
}
