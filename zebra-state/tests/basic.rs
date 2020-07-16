use color_eyre::eyre::Report;
use once_cell::sync::Lazy;
use std::sync::Arc;
use tempdir::TempDir;
use zebra_chain::{block::Block, serialization::ZcashDeserialize};
use zebra_test::transcript::Transcript;

use zebra_state::*;

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type ErrorChecker = fn(Error) -> Result<(), Error>;

static ADD_BLOCK_TRANSCRIPT: Lazy<Vec<(Request, Result<Response, ErrorChecker>)>> =
    Lazy::new(|| {
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
                Ok(Response::Added { hash }),
            ),
            (Request::GetBlock { hash }, Ok(Response::Block { block })),
        ]
    });

static GET_TIP_TRANSCRIPT: Lazy<Vec<(Request, Result<Response, ErrorChecker>)>> = Lazy::new(|| {
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
async fn check_transcripts() -> Result<(), Report> {
    zebra_test::init();

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
