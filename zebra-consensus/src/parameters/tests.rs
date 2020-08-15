//! Consensus parameter tests for Zebra.

use super::*;

use zebra_chain::{
    block::BlockHeight,
    parameters::Network::{self, *},
};

#[test]
fn minimum_difficulty_mainnet() {
    minimum_difficulty(Mainnet)
}

#[test]
fn minimum_difficulty_testnet() {
    minimum_difficulty(Testnet)
}

/// Test MinimumDifficulty
fn minimum_difficulty(network: Network) {
    use MinimumDifficulty::*;

    let allowed_if_testnet = match network {
        Mainnet => Rejected,
        Testnet => AllowedOnTestnet,
    };

    assert_eq!(
        MinimumDifficulty::current(network, BlockHeight(0)),
        Rejected
    );
    assert_eq!(
        MinimumDifficulty::current(network, BlockHeight(299_187)),
        Rejected
    );
    assert_eq!(
        MinimumDifficulty::current(network, BlockHeight(299_188)),
        allowed_if_testnet
    );
    assert_eq!(
        MinimumDifficulty::current(network, BlockHeight(299_189)),
        allowed_if_testnet
    );
    assert_eq!(
        MinimumDifficulty::current(network, BlockHeight::MAX),
        allowed_if_testnet
    );
}
