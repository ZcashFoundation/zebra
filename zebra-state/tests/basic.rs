use color_eyre::eyre::Report;
use once_cell::sync::Lazy;
use std::sync::Arc;
use tempdir::TempDir;
use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserialize};
use zebra_test::transcript::{TransError, Transcript};

use zebra_state::*;

static ADD_BLOCK_TRANSCRIPT_MAINNET: Lazy<Vec<(Request, Result<Response, TransError>)>> =
    Lazy::new(|| {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let hash = block.as_ref().into();
        vec![
            (
                Request::AddBlock {
                    block: block.clone(),
                },
                Ok(Response::Added { hash }),
            ),
            (Request::GetBlock { hash }, Ok(Response::Block { block })),
        ]
    });

static ADD_BLOCK_TRANSCRIPT_TESTNET: Lazy<Vec<(Request, Result<Response, TransError>)>> =
    Lazy::new(|| {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let hash = block.as_ref().into();
        vec![
            (
                Request::AddBlock {
                    block: block.clone(),
                },
                Ok(Response::Added { hash }),
            ),
            (Request::GetBlock { hash }, Ok(Response::Block { block })),
        ]
    });

static GET_TIP_TRANSCRIPT_MAINNET: Lazy<Vec<(Request, Result<Response, TransError>)>> =
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
        vec![
            // Insert higher block first, lower block second
            (
                Request::AddBlock { block: block1 },
                Ok(Response::Added { hash: hash1 }),
            ),
            (
                Request::AddBlock { block: block0 },
                Ok(Response::Added { hash: hash0 }),
            ),
            (Request::GetTip, Ok(Response::Tip { hash: hash1 })),
        ]
    });

static GET_TIP_TRANSCRIPT_TESTNET: Lazy<Vec<(Request, Result<Response, TransError>)>> =
    Lazy::new(|| {
        let block0: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let block1: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_TESTNET_1_BYTES[..])
                .unwrap()
                .into();
        let hash0 = block0.as_ref().into();
        let hash1 = block1.as_ref().into();
        vec![
            // Insert higher block first, lower block second
            (
                Request::AddBlock { block: block1 },
                Ok(Response::Added { hash: hash1 }),
            ),
            (
                Request::AddBlock { block: block0 },
                Ok(Response::Added { hash: hash0 }),
            ),
            (Request::GetTip, Ok(Response::Tip { hash: hash1 })),
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

    let mainnet_transcript = &[&ADD_BLOCK_TRANSCRIPT_TESTNET, &GET_TIP_TRANSCRIPT_TESTNET];
    let testnet_transcript = &[&ADD_BLOCK_TRANSCRIPT_MAINNET, &GET_TIP_TRANSCRIPT_MAINNET];

    for transcript_data in match network {
        Network::Testnet => testnet_transcript,
        _ => mainnet_transcript,
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
