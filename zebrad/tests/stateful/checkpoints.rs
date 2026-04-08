use color_eyre::eyre::Result;

use zebra_chain::parameters::Network::{self, *};

use crate::common::checkpoints;

/// Test `zebra-checkpoints` on mainnet.
///
/// If you want to run this test individually, see the module documentation.
/// See [`crate::common::checkpoints`] for more information.
#[tokio::test]
#[ignore]
async fn generate_checkpoints_mainnet() -> Result<()> {
    checkpoints::run(Mainnet).await
}

/// Test `zebra-checkpoints` on testnet.
/// This test might fail if testnet is unstable.
///
/// If you want to run this test individually, see the module documentation.
/// See [`crate::common::checkpoints`] for more information.
#[tokio::test]
#[ignore]
async fn generate_checkpoints_testnet() -> Result<()> {
    checkpoints::run(Network::new_default_testnet()).await
}
