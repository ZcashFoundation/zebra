use crate::{
    block,
    chain_tip::{mock::MockChainTip, ChainTip, AT_OR_NEAR_TIP_THRESHOLD},
    parameters::Network,
};

/// Check that the at-or-near-tip threshold is inclusive.
#[test]
fn at_or_near_network_tip_threshold_is_inclusive() {
    let network = Network::Mainnet;
    let (chain_tip, mock_chain_tip_sender) = MockChainTip::new();

    mock_chain_tip_sender.send_best_tip_height(block::Height(2_500_000));
    mock_chain_tip_sender.send_estimated_distance_to_network_chain_tip(AT_OR_NEAR_TIP_THRESHOLD);

    assert!(chain_tip.is_at_or_near_network_tip(&network));

    mock_chain_tip_sender
        .send_estimated_distance_to_network_chain_tip(AT_OR_NEAR_TIP_THRESHOLD + 1);

    assert!(!chain_tip.is_at_or_near_network_tip(&network));
}
