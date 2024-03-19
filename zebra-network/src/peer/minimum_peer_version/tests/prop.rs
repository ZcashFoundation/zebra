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
        let (mut minimum_peer_version, best_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&network);

        best_tip.send_best_tip_height(block_height);

        let expected_minimum_version = Version::min_remote_for_height(&network, block_height);

        prop_assert_eq!(minimum_peer_version.current(), expected_minimum_version);
    }

    /// Test if the calculated minimum peer version changes with the tip height.
    #[test]
    fn minimum_peer_version_is_updated_with_chain_tip(
        network in any::<Network>(),
        block_heights in any::<Vec<Option<block::Height>>>(),
    ) {
        let (mut minimum_peer_version, best_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&network);

        for block_height in block_heights {
            best_tip.send_best_tip_height(block_height);

            let expected_minimum_version = Version::min_remote_for_height(&network, block_height);

            prop_assert_eq!(minimum_peer_version.current(), expected_minimum_version);
        }
    }

    /// Test if the minimum peer version changes are correctly tracked.
    #[test]
    fn minimum_peer_version_reports_changes_correctly(
        network in any::<Network>(),
        block_height_updates in any::<Vec<Option<Option<block::Height>>>>(),
    ) {
        let (mut minimum_peer_version, best_tip) =
            MinimumPeerVersion::with_mock_chain_tip(&network);

        let mut current_minimum_version = Version::min_remote_for_height(&network, None);
        let mut expected_minimum_version = Some(current_minimum_version);

        prop_assert_eq!(minimum_peer_version.changed(), expected_minimum_version);

        for update in block_height_updates {
            if let Some(new_block_height) = update {
                best_tip.send_best_tip_height(new_block_height);

                let new_minimum_version = Version::min_remote_for_height(&network, new_block_height);

                expected_minimum_version = if new_minimum_version != current_minimum_version {
                    Some(new_minimum_version)
                } else {
                    None
                };

                current_minimum_version = new_minimum_version;
            } else {
                expected_minimum_version = None;
            }

            prop_assert_eq!(minimum_peer_version.changed(), expected_minimum_version);
        }
    }
}
