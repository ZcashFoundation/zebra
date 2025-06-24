//! Response type for the `validateaddress` RPC.

use derive_getters::Getters;
use derive_new::new;

/// `validateaddress` response
#[derive(
    Clone, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Getters, new,
)]
pub struct ValidateAddressResponse {
    /// Whether the address is valid.
    ///
    /// If not, this is the only property returned.
    #[serde(rename = "isvalid")]
    pub(crate) is_valid: bool,

    /// The zcash address that has been validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) address: Option<String>,

    /// If the key is a script.
    #[serde(rename = "isscript")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[getter(copy)]
    pub(crate) is_script: Option<bool>,
}

impl ValidateAddressResponse {
    /// Creates an empty response with `isvalid` of false.
    pub fn invalid() -> Self {
        Self::default()
    }
}
