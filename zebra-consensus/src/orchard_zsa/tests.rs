use std::sync::Arc;

use color_eyre::eyre::Report;

use zebra_chain::{
    block::{genesis::regtest_genesis_block, Block, Hash},
    parameters::Network,
    serialization::ZcashDeserialize,
};

use zebra_test::{
    transcript::{ExpectedTranscriptError, Transcript},
    vectors::ZSA_WORKFLOW_BLOCKS,
};

use crate::{block::Request, Config};

fn create_transcript_data() -> impl Iterator<Item = (Request, Result<Hash, ExpectedTranscriptError>)>
{
    let workflow_blocks = ZSA_WORKFLOW_BLOCKS.iter().map(|block_bytes| {
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

    let (
        block_verifier_router,
        _transaction_verifier,
        _groth16_download_handle,
        _max_checkpoint_height,
    ) = crate::router::init(Config::default(), &network, state_service.clone()).await;

    Transcript::from(create_transcript_data())
        .check(block_verifier_router.clone())
        .await
}
