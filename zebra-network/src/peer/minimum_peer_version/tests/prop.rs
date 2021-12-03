use proptest::prelude::*;

use zebra_chain::{block, parameters::Network};

use crate::{peer::MinimumPeerVersion, protocol::external::types::Version};

proptest! {
    /// Test if the calculated minimum peer version is correct.
    #[test]
    fn minimum_peer_version_is_correct(
        network in any::<Network>(),
        block_height in any::<Option<block::Height>>(),
    ) {
        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(network);

        best_tip_height
            .send(block_height)
            .expect("receiving endpoint lives as long as `minimum_peer_version`");

        let expected_minimum_version = Version::min_remote_for_height(network, block_height);

        prop_assert_eq!(minimum_peer_version.current(), expected_minimum_version);
    }

    /// Test if the calculated minimum peer version changes with the tip height.
    #[test]
    fn minimum_peer_version_is_updated_with_chain_tip(
        network in any::<Network>(),
        block_heights in any::<Vec<Option<block::Height>>>(),
    ) {
        let (mut minimum_peer_version, best_tip_height) =
            MinimumPeerVersion::with_mock_chain_tip(network);

        for block_height in block_heights {
            best_tip_height
                .send(block_height)
                .expect("receiving endpoint lives as long as `minimum_peer_version`");

            let expected_minimum_version = Version::min_remote_for_height(network, block_height);

            prop_assert_eq!(minimum_peer_version.current(), expected_minimum_version);
        }
    }
}
