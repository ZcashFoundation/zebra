//! Test utilities and tests for minimum network peer version requirements.

use tokio::sync::watch;

use zebra_chain::{block, chain_tip::mock::MockChainTip, parameters::Network};

use super::MinimumPeerVersion;

#[cfg(test)]
mod prop;

impl MinimumPeerVersion<MockChainTip> {
    pub fn with_mock_chain_tip(network: Network) -> (Self, watch::Sender<Option<block::Height>>) {
        let (chain_tip, best_tip_height) = MockChainTip::new();
        let minimum_peer_version = MinimumPeerVersion::new(chain_tip, network);

        (minimum_peer_version, best_tip_height)
    }
}
