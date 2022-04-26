//! Basic integration tests for zebra-state

use std::sync::Arc;

use color_eyre::eyre::Report;
use once_cell::sync::Lazy;

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserialize};
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use zebra_state::*;

static COMMIT_FINALIZED_BLOCK_MAINNET: Lazy<
    Vec<(Request, Result<Response, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let block2 = block.clone();
    let hash = block.hash();
    vec![
        (
            Request::CommitFinalizedBlock(block.into()),
            Ok(Response::Committed(hash)),
        ),
        (
            Request::Block(hash.into()),
            Ok(Response::Block(Some(block2))),
        ),
    ]
});

static COMMIT_FINALIZED_BLOCK_TESTNET: Lazy<
    Vec<(Request, Result<Response, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let block2 = block.clone();
    let hash = block.hash();
    vec![
        (
            Request::CommitFinalizedBlock(block.into()),
            Ok(Response::Committed(hash)),
        ),
        (
            Request::Block(hash.into()),
            Ok(Response::Block(Some(block2))),
        ),
    ]
});

#[tokio::test]
async fn check_transcripts_mainnet() -> Result<(), Report> {
    check_transcripts(Network::Mainnet).await
}

#[tokio::test]
async fn check_transcripts_testnet() -> Result<(), Report> {
    check_transcripts(Network::Testnet).await
}

#[spandoc::spandoc]
async fn check_transcripts(network: Network) -> Result<(), Report> {
    zebra_test::init();

    let mainnet_transcript = &[&COMMIT_FINALIZED_BLOCK_MAINNET];
    let testnet_transcript = &[&COMMIT_FINALIZED_BLOCK_TESTNET];

    for transcript_data in match network {
        Network::Mainnet => mainnet_transcript,
        Network::Testnet => testnet_transcript,
    } {
        let (service, _, _, _) = zebra_state::init(Config::ephemeral(), network);
        let transcript = Transcript::from(transcript_data.iter().cloned());
        /// SPANDOC: check the on disk service against the transcript
        transcript.check(service).await?;
    }

    Ok(())
}
