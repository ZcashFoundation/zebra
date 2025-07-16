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
///
/// # Parameters
///
/// - `network: Network`
///   - The network (Mainnet or Testnet) against which the address validation is performed.
///
/// - `raw_address: String`
///   - The raw input address as a string that needs to be validated.
///
/// # Returns
///
/// - `RpcResult<ZValidateAddressResponse>`
///   - A result containing a `ZValidateAddressResponse` object if the address validation succeeds,
///     or an error if the validation process encounters an issue.
///
/// # Process
///
/// 1. **Parse the Raw Address**:
///    Attempts to parse the raw address into a `zcash_address::ZcashAddress`. If parsing fails,
///    returns an invalid address response.
///
/// 2. **Convert Address**:
///    If parsing is successful, attempts to convert the address to the `Address` type.
///    If conversion fails, logs the conversion error and returns an invalid response.
///
/// 3. **Network Validation**:
///    Checks if the parsed address belongs to the given network. If not, logs an info message
///    about the network mismatch and returns an invalid response.
///
/// 4. **Generate Response**:
///    If all validations pass, constructs and returns a `ZValidateAddressResponse` with:
///      - `is_valid` set to `true`,
///      - the given `raw_address`,
///      - inferred `address_type`,
///      - `is_mine` set to `false` (as wallet functionality is not implemented).
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
