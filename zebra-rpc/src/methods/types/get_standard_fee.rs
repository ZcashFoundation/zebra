//! Types for the `getstandardfee` RPC.

use derive_getters::Getters;
use derive_new::new;

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
