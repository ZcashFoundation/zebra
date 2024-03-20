use proptest::prelude::*;

use super::super::{Network, ZIP_212_GRACE_PERIOD_DURATION};
use crate::{
    block::Height,
    parameters::{NetworkUpgrade, TESTNET_MAX_TIME_START_HEIGHT},
};

proptest! {
    /// Check that the mandatory checkpoint is after the ZIP-212 grace period.
    ///
    /// This is necessary because Zebra can't fully validate the blocks during the grace period due
    /// to a limitation of `librustzcash`.
    ///
    /// See [`ZIP_212_GRACE_PERIOD_DURATION`] for more information.
    #[test]
    fn mandatory_checkpoint_is_after_zip212_grace_period(network in any::<Network>()) {
        let _init_guard = zebra_test::init();

        let canopy_activation = NetworkUpgrade::Canopy
            .activation_height(&network)
            .expect("Canopy activation height is set");

        let grace_period_end_height = (canopy_activation + ZIP_212_GRACE_PERIOD_DURATION)
            .expect("ZIP-212 grace period ends in a valid block height");

        assert!(network.mandatory_checkpoint_height() >= grace_period_end_height);
    }

    #[test]
    /// Asserts that the activation height is correct for the block
    /// maximum time rule on Testnet is correct.
    fn max_block_times_correct_enforcement(height in any::<Height>()) {
        let _init_guard = zebra_test::init();

        assert!(Network::Mainnet.is_max_block_time_enforced(height));
        assert_eq!(Network::new_default_testnet().is_max_block_time_enforced(height), TESTNET_MAX_TIME_START_HEIGHT <= height);
    }
}
