//! Types for the `getstandardfee` RPC.

use derive_getters::Getters;
use derive_new::new;

use zebra_chain::{block::Height, parameters::Network, transaction::zip317::MARGINAL_FEE};

/// The ZIP 317 marginal fee wallets paid before [`MARGINAL_FEE`] was lowered, in zatoshis per
/// logical action.
const LEGACY_MARGINAL_FEE: u64 = 5_000;

/// The Mainnet height from which `getstandardfee` reports [`MARGINAL_FEE`] instead of
/// [`LEGACY_MARGINAL_FEE`].
///
/// The mempool accepts [`MARGINAL_FEE`] as soon as a node upgrades, but nodes that have not
/// upgraded drop those transactions. The [draft ZIP] has wallets switch at this height, after
/// the end-of-support halt of Zebra v6.3.0 (height 3,564,960), so that they switch together
/// instead of revealing their wallet software by the fee they pay.
///
/// [draft ZIP]: https://github.com/zcash/zips/pull/1352
const MAINNET_STANDARD_FEE_ACTIVATION_HEIGHT: Height = Height(3_590_000);

/// Returns the fee per logical action, in zatoshis, that wallets should pay for a transaction
/// mined in the block at `height` on `network`.
///
/// Test networks report [`MARGINAL_FEE`] at every height, because the draft ZIP only schedules
/// the switch on Mainnet.
pub(crate) fn standard_fee(network: &Network, height: Height) -> u64 {
    if network.is_a_test_network() || height >= MAINNET_STANDARD_FEE_ACTIVATION_HEIGHT {
        MARGINAL_FEE
    } else {
        LEGACY_MARGINAL_FEE
    }
}

/// A response to a `getstandardfee` RPC request.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct GetStandardFeeResponse {
    /// Recommended fee per logical action, in zatoshis.
    #[getter(copy)]
    pub(crate) standard_fee: u64,

    /// Estimator version identifier.
    #[getter(copy)]
    pub(crate) version: u32,
}
