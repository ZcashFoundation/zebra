use proptest::prelude::*;

use zebra_chain::{
    block,
    parameters::{Network, NetworkUpgrade},
};

/// Generate an arbitrary block height after the Sapling activation height on an arbitrary network.
///
/// A proptest [`Strategy`] that generates random tuples with
///
/// - a network (mainnet or testnet)
/// - a block height between the Sapling activation height (inclusive) on that network and the
///   maximum block height.
fn sapling_onwards_strategy() -> impl Strategy<Value = (Network, block::Height)> {
    any::<Network>().prop_flat_map(|network| {
        let start_height_value = NetworkUpgrade::Sapling
            .activation_height(network)
            .expect("Sapling to have an activation height")
            .0;

        let end_height_value = block::Height::MAX.0;

        (start_height_value..=end_height_value)
            .prop_map(move |height_value| (network, block::Height(height_value)))
    })
}
