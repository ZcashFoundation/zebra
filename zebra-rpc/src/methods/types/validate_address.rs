//! Response type for the `validateaddress` RPC.

/// `validateaddress` response
#[derive(Clone, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Response {
    /// Whether the address is valid.
    ///
    /// If not, this is the only property returned.
    #[serde(rename = "isvalid")]
    pub is_valid: bool,

    /// The zcash address that has been validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,

    /// If the key is a script.
    #[serde(rename = "isscript")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_script: Option<bool>,
}

impl Response {
    /// Creates an empty response with `isvalid` of false.
    pub fn invalid() -> Self {
        Self::default()
    }
}
