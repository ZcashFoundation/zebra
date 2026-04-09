//! Test utilities and tests for minimum network peer version requirements.

#![allow(clippy::unwrap_in_result)]
#![cfg_attr(feature = "proptest-impl", allow(dead_code))]

use zebra_chain::{
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::Network,
};

use super::MinimumPeerVersion;

#[cfg(test)]
mod prop;

impl MinimumPeerVersion<MockChainTip> {
    pub fn with_mock_chain_tip(network: &Network) -> (Self, MockChainTipSender) {
        let (chain_tip, best_tip) = MockChainTip::new();
        let minimum_peer_version = MinimumPeerVersion::new(chain_tip, network);

        (minimum_peer_version, best_tip)
    }
}
