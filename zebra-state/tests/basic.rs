//! Basic integration tests for zebra-state

#![allow(clippy::unwrap_in_result)]

use std::sync::Arc;

use color_eyre::eyre::Report;
use once_cell::sync::Lazy;

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserialize,
};
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
            Request::CommitCheckpointVerifiedBlock(block.into()),
            Ok(Response::Committed(hash)),
        ),
        (
            Request::Read(ReadRequest::BlockQuery(BlockQuery {
                hash_or_height: hash.into(),
                chain: ChainSelector::Best,
                fields: [BlockField::Block].into(),
            })),
            Ok(Response::Read(ReadResponse::BlockQuery(Some(
                QueriedBlock {
                    block: Some(block2),
                    ..QueriedBlock::default()
                },
            )))),
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
            Request::CommitCheckpointVerifiedBlock(block.into()),
            Ok(Response::Committed(hash)),
        ),
        (
            Request::Read(ReadRequest::BlockQuery(BlockQuery {
                hash_or_height: hash.into(),
                chain: ChainSelector::Best,
                fields: [BlockField::Block].into(),
            })),
            Ok(Response::Read(ReadResponse::BlockQuery(Some(
                QueriedBlock {
                    block: Some(block2),
                    ..QueriedBlock::default()
                },
            )))),
        ),
    ]
});

#[tokio::test(flavor = "multi_thread")]
async fn check_transcripts_mainnet() -> Result<(), Report> {
    check_transcripts(Network::Mainnet).await
}

#[tokio::test(flavor = "multi_thread")]
async fn check_transcripts_testnet() -> Result<(), Report> {
    check_transcripts(Network::new_default_testnet()).await
}

#[spandoc::spandoc]
async fn check_transcripts(network: Network) -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let mainnet_transcript = &[&COMMIT_FINALIZED_BLOCK_MAINNET];
    let testnet_transcript = &[&COMMIT_FINALIZED_BLOCK_TESTNET];

    let net_data = if network.is_mainnet() {
        mainnet_transcript
    } else {
        testnet_transcript
    };

    for transcript_data in net_data {
        // We're not verifying UTXOs here.
        let (service, _, _, _) =
            zebra_state::init(Config::ephemeral(), &network, Height::MAX, 0).await;
        let transcript = Transcript::from(transcript_data.iter().cloned());
        /// SPANDOC: check the on disk service against the transcript
        transcript.check(service).await?;
    }

    Ok(())
}
