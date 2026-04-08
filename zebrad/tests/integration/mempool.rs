use color_eyre::eyre::Result;

use zebra_chain::{block, parameters::Network::*};

use crate::common::sync::{
    sync_until, MempoolBehavior, STOP_AT_HEIGHT_REGEX, TINY_CHECKPOINT_TEST_HEIGHT,
    TINY_CHECKPOINT_TIMEOUT,
};

/// Test if `zebrad` can activate the mempool on mainnet.
/// Debug activation happens after committing the genesis block.
#[test]
fn activate_mempool_mainnet() -> Result<()> {
    sync_until(
        block::Height(TINY_CHECKPOINT_TEST_HEIGHT.0 + 1),
        &Mainnet,
        STOP_AT_HEIGHT_REGEX,
        TINY_CHECKPOINT_TIMEOUT,
        None,
        MempoolBehavior::ForceActivationAt(TINY_CHECKPOINT_TEST_HEIGHT),
        // checkpoint sync is irrelevant here - all tested checkpoints are mandatory
        true,
        true,
    )
    .map(|_tempdir| ())
}
