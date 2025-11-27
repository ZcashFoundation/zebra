//! Simulates a full Zebra node’s block‐processing pipeline on a predefined Orchard/ZSA workflow.
//!
//! This integration test reads a sequence of serialized regtest blocks (including Orchard burns
//! and ZSA issuance), feeds them through the node’s deserialization, consensus router, and state
//! service exactly as if they arrived from the network, and verifies that each block is accepted
//! (or fails at the injected point).
//!
//! In a future PR, we will add tracking and verification of issuance/burn state changes so that
//! the test can also assert that on-chain asset state (total supply and finalization flags)
//! matches the expected values computed in memory.
//!
//! In short, it demonstrates end-to-end handling of Orchard asset burns and ZSA issuance through
//! consensus (with state verification to follow in the next PR).

use std::sync::Arc;

use color_eyre::eyre::{eyre, Report};
use tokio::time::{timeout, Duration};

use zebra_chain::{
    block::{genesis::regtest_genesis_block, Block, Hash},
    parameters::Network,
    serialization::ZcashDeserialize,
};

use zebra_test::{
    transcript::{ExpectedTranscriptError, Transcript},
    vectors::ORCHARD_ZSA_WORKFLOW_BLOCKS,
};

use crate::{block::Request, Config};

fn create_transcript_data() -> impl Iterator<Item = (Request, Result<Hash, ExpectedTranscriptError>)>
{
    let workflow_blocks = ORCHARD_ZSA_WORKFLOW_BLOCKS.iter().map(|block_bytes| {
        Arc::new(Block::zcash_deserialize(&block_bytes[..]).expect("block should deserialize"))
    });

    std::iter::once(regtest_genesis_block())
        .chain(workflow_blocks)
        .map(|block| (Request::Commit(block.clone()), Ok(block.hash())))
}

#[tokio::test(flavor = "multi_thread")]
async fn check_zsa_workflow() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Some(1), Some(1), Some(1));

    let state_service = zebra_state::init_test(&network);

    let (block_verifier_router, ..) =
        crate::router::init(Config::default(), &network, state_service).await;

    timeout(
        Duration::from_secs(15),
        Transcript::from(create_transcript_data()).check(block_verifier_router),
    )
    .await
    .map_err(|_| eyre!("Task timed out"))?
}
