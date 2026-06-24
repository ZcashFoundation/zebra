//! Types used in the `getreadstateinfo` RPC method.

use derive_getters::Getters;
use derive_new::new;

use zebra_chain::parameters::{
    testnet::ConfiguredActivationHeights, Network, NetworkKind, NetworkUpgrade,
};

/// Network identity reported to a co-located read-state follower.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
#[serde(rename_all = "camelCase")]
pub struct ReadStateNetworkInfo {
    /// `"Mainnet"`, `"Testnet"`, or `"Regtest"`.
    kind: String,
    /// The network's genesis block hash, in display order.
    genesis_hash: String,
    /// Configured network-upgrade activation heights. Present only for Regtest, where the
    /// follower must reconstruct them; absent for Mainnet/Testnet, whose heights are fixed
    /// by the network kind.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    regtest_activation_heights: Option<ConfiguredActivationHeights>,
}

/// The response to a `getreadstateinfo` request: everything a co-located follower needs to
/// open a read-only secondary of this node's state and follow its non-finalized chain.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
#[serde(rename_all = "camelCase")]
pub struct GetReadStateInfo {
    /// The node's indexer gRPC listen address, or `None` if the indexer is not enabled.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    indexer_grpc_addr: Option<String>,
    /// The live on-disk path of the node's finalized-state database.
    state_db_path: String,
    /// The database format version of the running node.
    db_format_version: String,
    /// The database kind (e.g. `"state"`).
    db_kind: String,
    /// The node's network identity.
    network: ReadStateNetworkInfo,
}

/// Builds the reported network identity from a [`Network`], including Regtest activation
/// heights when applicable.
pub fn network_info(network: &Network) -> ReadStateNetworkInfo {
    let kind = match network.kind() {
        NetworkKind::Mainnet => "Mainnet",
        NetworkKind::Testnet => "Testnet",
        NetworkKind::Regtest => "Regtest",
    }
    .to_string();

    let regtest_activation_heights = network.is_regtest().then(|| {
        let h = |nu: NetworkUpgrade| nu.activation_height(network).map(|height| height.0);
        // `..Default::default()` covers the cfg-gated `zfuture` field without a cfg here.
        #[allow(clippy::needless_update)]
        ConfiguredActivationHeights {
            before_overwinter: h(NetworkUpgrade::BeforeOverwinter),
            overwinter: h(NetworkUpgrade::Overwinter),
            sapling: h(NetworkUpgrade::Sapling),
            blossom: h(NetworkUpgrade::Blossom),
            heartwood: h(NetworkUpgrade::Heartwood),
            canopy: h(NetworkUpgrade::Canopy),
            nu5: h(NetworkUpgrade::Nu5),
            nu6: h(NetworkUpgrade::Nu6),
            nu6_1: h(NetworkUpgrade::Nu6_1),
            nu6_2: h(NetworkUpgrade::Nu6_2),
            nu7: h(NetworkUpgrade::Nu7),
            ..Default::default()
        }
    });

    ReadStateNetworkInfo::new(
        kind,
        network.genesis_hash().to_string(),
        regtest_activation_heights,
    )
}
