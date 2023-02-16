//! Response type for the `z_validateaddress` RPC.

use zebra_chain::primitives::Address;

/// `z_validateaddress` response
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

    /// The type of the address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_type: Option<AddressType>,

    /// Whether the address is yours or not.
    ///
    /// Always false for now since Zebra doesn't have a wallet yet.
    #[serde(rename = "ismine")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_mine: Option<bool>,
}

impl Response {
    /// Creates an empty response with `isvalid` of false.
    pub fn invalid() -> Self {
        Self::default()
    }
}

/// Address types supported by the `z_validateaddress` RPC according to
/// <https://zcash.github.io/rpc/z_validateaddress.html>.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AddressType {
    /// The `p2pkh` address type.
    P2pkh,
    /// The `p2sh` address type.
    P2sh,
    /// The `sapling` address type.
    Sapling,
    /// The `unified` address type.
    Unified,
}

impl From<&Address> for AddressType {
    fn from(address: &Address) -> Self {
        match address {
            Address::Transparent(_) => {
                if address.is_script_hash() {
                    Self::P2sh
                } else {
                    Self::P2pkh
                }
            }
            Address::Sapling { .. } => Self::Sapling,
            Address::Unified { .. } => Self::Unified,
        }
    }
}
