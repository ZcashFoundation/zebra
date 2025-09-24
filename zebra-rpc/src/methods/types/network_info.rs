//! Types used in the `getnetworkinfo` RPC method

use derive_new::new;

/// Response to a `getnetworkinfo` RPC request
///
/// See the notes for [`Rpc::get_network_info` method]
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetNetworkInfoResponse {
    /// The server version
    pub version: u64,

    /// The server sub-version string
    pub subversion: String,

    /// The protocol version
    #[serde(rename = "protocolversion")]
    pub protocol_version: u32,

    /// The services we offer to the network
    #[serde(rename = "localservices")]
    pub local_services: String,

    /// The time offset (deprecated; always 0)
    pub timeoffset: i64,

    /// The total number of connections
    pub connections: usize,

    /// Information per network
    pub networks: Vec<NetworkInfo>,

    /// Minimum relay fee rate for transactions in ZEC per 1000 bytes
    #[serde(rename = "relayfee")]
    pub relay_fee: f64,

    /// List of local network addresses
    #[serde(rename = "localaddresses")]
    pub local_addresses: Vec<LocalAddress>,

    /// Any network warnings (such as alert messages)
    pub warnings: String,
}

/// Information about a specific network (ipv4, ipv6, onion).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, new)]
pub struct NetworkInfo {
    /// Network (ipv4, ipv6 or onion)
    pub name: String,

    /// Is the network limited using -onlynet?
    pub limited: bool,

    /// Is the network reachable?
    pub reachable: bool,

    /// The proxy that is used for this network, or empty if none
    pub proxy: String,

    /// Whether to randomize credentials for the proxy (present in zcashd, undocumented)
    pub proxy_randomize_credentials: bool,
}

/// Local address info.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LocalAddress {
    /// Network address
    pub address: String,

    /// Network port
    pub port: u16,

    /// Relative score
    pub score: u32,
}
