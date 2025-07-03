//! Response type for the `validateaddress` RPC.

use derive_getters::Getters;
use derive_new::new;
use jsonrpsee::core::RpcResult;
use zebra_chain::{parameters::Network, primitives};

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

/// Checks if a zcash transparent address of type P2PKH, P2SH or TEX is valid.
/// Returns information about the given address if valid.
pub fn validate_address(
    network: Network,
    raw_address: String,
) -> RpcResult<ValidateAddressResponse> {
    let Ok(address) = raw_address.parse::<zcash_address::ZcashAddress>() else {
        return Ok(ValidateAddressResponse::invalid());
    };

    let address = match address.convert::<primitives::Address>() {
        Ok(address) => address,
        Err(err) => {
            tracing::debug!(?err, "conversion error");
            return Ok(ValidateAddressResponse::invalid());
        }
    };

    // We want to match zcashd's behaviour
    if !address.is_transparent() {
        return Ok(ValidateAddressResponse::invalid());
    }

    if address.network() == network.kind() {
        Ok(ValidateAddressResponse {
            address: Some(raw_address),
            is_valid: true,
            is_script: Some(address.is_script_hash()),
        })
    } else {
        tracing::info!(
            ?network,
            address_network = ?address.network(),
            "invalid address in validateaddress RPC: Zebra's configured network must match address network"
        );

        Ok(ValidateAddressResponse::invalid())
    }
}
