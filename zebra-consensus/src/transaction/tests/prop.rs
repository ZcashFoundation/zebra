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

/// Selects a [`block::Height`] between `min_height` and `max_height` using the `scale` factor.
///
/// The `scale` must be in the range `0.0..1.0`, where `0.0` results in the selection of
/// `min_height` and `1.0` would select the `max_height` if the range was inclusive. The range is
/// exclusive however, so `max_height` is never selected (unless it is equal to `min_height`).
///
/// # Panics
///
/// - if `scale` is not in the range `0.0..1.0`
/// - if `min_height` is greater than `max_height`
fn scale_block_height(
    min_height: impl Into<Option<block::Height>>,
    max_height: impl Into<Option<block::Height>>,
    scale: f64,
) -> block::Height {
    assert!(scale >= 0.0);
    assert!(scale < 1.0);

    let min_height = min_height.into().unwrap_or(block::Height(0));
    let max_height = max_height.into().unwrap_or(block::Height::MAX);

    assert!(min_height <= max_height);

    let min_height_value = f64::from(min_height.0);
    let max_height_value = f64::from(max_height.0);
    let height_range = max_height_value - min_height_value;

    let new_height_value = (height_range * scale + min_height_value).floor();

    block::Height(new_height_value as u32)
}
