use color_eyre::eyre::Report;
use color_eyre::eyre::{bail, ensure, eyre};
use once_cell::sync::Lazy;
use std::sync::{Arc, Once};
use tempdir::TempDir;
use tower::Service;
use tracing_error::ErrorLayer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    serialization::ZcashDeserialize,
};
use zebra_test::transcript::Transcript;

use zebra_state::*;

static LOGGER_INIT: Once = Once::new();

fn install_tracing() {
    LOGGER_INIT.call_once(|| {
        let fmt_layer = fmt::layer().with_target(false);
        let filter_layer = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap();

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .with(ErrorLayer::default())
            .init();
    })
}

static ADD_BLOCK_TRANSCRIPT: Lazy<Vec<(Request, Response)>> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .unwrap()
            .into();
    let hash = block.as_ref().into();
    vec![
        (
            Request::AddBlock {
                block: block.clone(),
            },
            Response::Added { hash },
        ),
        (Request::GetBlock { hash }, Response::Block { block }),
    ]
});

static GET_TIP_TRANSCRIPT: Lazy<Vec<(Request, Response)>> = Lazy::new(|| {
    let block0: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let block1: Arc<_> = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
        .unwrap()
        .into();
    let hash0 = block0.as_ref().into();
    let hash1 = block1.as_ref().into();
    vec![
        // Insert higher block first, lower block second
        (
            Request::AddBlock { block: block1 },
            Response::Added { hash: hash1 },
        ),
        (
            Request::AddBlock { block: block0 },
            Response::Added { hash: hash0 },
        ),
        (Request::GetTip, Response::Tip { hash: hash1 }),
    ]
});

#[tokio::test]
async fn check_transcripts() -> Result<(), Report> {
    install_tracing();

    for transcript_data in &[&ADD_BLOCK_TRANSCRIPT, &GET_TIP_TRANSCRIPT] {
        let service = in_memory::init();
        let transcript = Transcript::from(transcript_data.iter().cloned());
        transcript.check(service).await?;

        let storage_guard = TempDir::new("")?;
        let service = on_disk::init(Config {
            path: storage_guard.path().to_owned(),
        });
        let transcript = Transcript::from(transcript_data.iter().cloned());
        transcript.check(service).await?;
        // Delete the contents of the temp directory before going to the next case.
        std::mem::drop(storage_guard);
    }

    Ok(())
}
