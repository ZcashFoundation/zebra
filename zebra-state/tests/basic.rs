use color_eyre::eyre::Report;
use once_cell::sync::Lazy;
use std::sync::Arc;
use tempdir::TempDir;
use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserialize};
use zebra_test::transcript::{TransError, Transcript};

use zebra_state::*;

static COMMIT_FINALIZED_BLOCK_MAINNET: Lazy<Vec<(Request, Result<Response, TransError>)>> =
    Lazy::new(|| {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let block2 = block.clone();
        let hash = block.hash();
        vec![
            (
                Request::CommitFinalizedBlock { block },
                Ok(Response::Committed(hash)),
            ),
            (
                Request::Block(hash.into()),
                Ok(Response::Block(Some(block2))),
            ),
        ]
    });

static GET_TIP_ADD_REVERSED_TRANSCRIPT_MAINNET: Lazy<Vec<(Request, Result<Response, TransError>)>> =
    Lazy::new(|| {
        let block0: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let block1: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
                .unwrap()
                .into();
        let hash0 = block0.as_ref().into();
        let hash1 = block1.as_ref().into();
        let height1 = block1.coinbase_height().unwrap();
        vec![
            // Insert the blocks in reverse order
            (
                Request::CommitFinalizedBlock { block: block1 },
                Ok(Response::Committed(hash1)),
            ),
            (
                Request::CommitFinalizedBlock { block: block0 },
                Ok(Response::Committed(hash0)),
            ),
            (Request::Tip, Ok(Response::Tip(Some((height1, hash1))))),
        ]
    });

static GET_TIP_ADD_REVERSED_TRANSCRIPT_TESTNET: Lazy<Vec<(Request, Result<Response, TransError>)>> =
    Lazy::new(|| {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let block2 = block.clone();
        let hash = block.hash();
        vec![
            (
                Request::CommitFinalizedBlock { block },
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

    let mainnet_transcript: &[_] = &[
        &COMMIT_FINALIZED_BLOCK_MAINNET,
        &GET_TIP_ADD_REVERSED_TRANSCRIPT_MAINNET,
    ];
    let testnet_transcript: &[_] = &[&GET_TIP_ADD_REVERSED_TRANSCRIPT_TESTNET];

    for transcript_data in match network {
        Network::Mainnet => mainnet_transcript,
        Network::Testnet => testnet_transcript,
    } {
        let storage_guard = TempDir::new("")?;
        let cache_dir = storage_guard.path().to_owned();
        let service = zebra_state::init(
            Config {
                cache_dir,
                ..Config::default()
            },
            network,
        );
        let transcript = Transcript::from(transcript_data.iter().cloned());
        /// SPANDOC: check the on disk service against the transcript
        transcript.check(service).await?;
        // Delete the contents of the temp directory before going to the next case.
        std::mem::drop(storage_guard);
    }

    Ok(())
}
