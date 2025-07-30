//! Response type for the `z_validateaddress` RPC.

use derive_getters::Getters;
use derive_new::new;
use jsonrpsee::core::RpcResult;
use zebra_chain::{parameters::Network, primitives::Address};

/// `z_validateaddress` response
#[derive(
    Clone, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Getters, new,
)]
pub struct ZValidateAddressResponse {
    /// Whether the address is valid.
    ///
    /// If not, this is the only property returned.
    #[serde(rename = "isvalid")]
    pub(crate) is_valid: bool,

    /// The zcash address that has been validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) address: Option<String>,

    /// The type of the address.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[getter(copy)]
    pub(crate) address_type: Option<ZValidateAddressType>,

    /// Whether the address is yours or not.
    ///
    /// Always false for now since Zebra doesn't have a wallet yet.
    #[serde(rename = "ismine")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[getter(copy)]
    pub(crate) is_mine: Option<bool>,
}

impl ZValidateAddressResponse {
    /// Creates an empty response with `isvalid` of false.
    pub fn invalid() -> Self {
        Self::default()
    }
}

/// Address types supported by the `z_validateaddress` RPC according to
/// <https://zcash.github.io/rpc/z_validateaddress.html>.
#[derive(Copy, Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ZValidateAddressType {
    /// The `p2pkh` address type.
    P2pkh,
    /// The `p2sh` address type.
    P2sh,
    /// The `sapling` address type.
    Sapling,
    /// The `unified` address type.
    Unified,
}

impl From<&Address> for ZValidateAddressType {
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

/// Validates a Zcash address against a network and returns a structured response.
pub fn z_validateaddress(
    network: Network,
    raw_address: String,
) -> RpcResult<ZValidateAddressResponse> {
    let Ok(address) = raw_address.parse::<zcash_address::ZcashAddress>() else {
        return Ok(ZValidateAddressResponse::invalid());
    };

    let address = match address.convert::<Address>() {
        Ok(address) => address,
        Err(err) => {
            tracing::debug!(?err, "conversion error");
            return Ok(ZValidateAddressResponse::invalid());
        }
    };

    if address.network() == network.kind() {
        Ok(ZValidateAddressResponse {
            is_valid: true,
            address: Some(raw_address),
            address_type: Some(ZValidateAddressType::from(&address)),
            is_mine: Some(false),
        })
    } else {
        tracing::info!(
            ?network,
            address_network = ?address.network(),
            "invalid address network in z_validateaddress RPC: address is for {:?} but Zebra is on {:?}",
            address.network(),
            network
        );

        Ok(ZValidateAddressResponse::invalid())
    }
}
