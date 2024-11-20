// FIXME: consider merging it with router/tests.rs

// FIXME: fix warnings, clippy, remove printlns and commented out lines

use std::sync::Arc;

use color_eyre::eyre::Report;

use zebra_chain::{
    block::{genesis::regtest_genesis_block, Block, Hash},
    orchard_zsa::tests::vectors::BLOCKS,
    parameters::Network,
    serialization::ZcashDeserialize,
};
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use crate::{block::Request, Config};

fn patch_block_coinbase_height(block: &mut Block, previous_block: &Block) {
    let proper_height = previous_block
        .coinbase_height()
        .expect("block has coinbase_height")
        .next()
        .expect("block has next coinbase_height");

    let mut transactions: Vec<Arc<zebra_chain::transaction::Transaction>> =
        block.transactions.iter().cloned().collect();
    //let transactions: &mut Vec<Arc<zebra_chain::transaction::Transaction>> =
    //    Arc::make_mut(&mut block.transactions);
    if let Some(tx_arc) = transactions.first_mut() {
        let tx = Arc::make_mut(&mut *tx_arc);

        if let Some(input) = tx.inputs_mut().first_mut() {
            if let zebra_chain::transparent::Input::Coinbase { ref mut height, .. } = input {
                *height = proper_height;
            }
        }
    }

    block.transactions = transactions.into();
}

fn patch_block(block: &mut Block, previous_block: &Block, commitment_bytes: [u8; 32]) {
    println!(
        "patch_block merkle_root before: {:?}",
        block.header.merkle_root
    );
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = previous_block.hash();
    *header.commitment_bytes = commitment_bytes;

    patch_block_coinbase_height(block, previous_block);

    let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|t| t.hash()).collect();
    let merkle_root = transaction_hashes.iter().cloned().collect();
    println!("patch_block merkle_root after: {:?}", merkle_root);
    let header = Arc::make_mut(&mut block.header);
    header.merkle_root = merkle_root;
}

fn create_transcript_data() -> [(Request, Result<Hash, ExpectedTranscriptError>); 4] {
    let genesis_block: Arc<Block> = regtest_genesis_block();

    let mut issuance_block: Block =
        Block::zcash_deserialize(BLOCKS[1].as_ref()).expect("issuance block should deserialize");
    let mut transfer_block: Block =
        Block::zcash_deserialize(BLOCKS[2].as_ref()).expect("transfer block should deserialize");
    let mut burn_block: Block =
        Block::zcash_deserialize(BLOCKS[3].as_ref()).expect("burn block should deserialize");

    patch_block(&mut issuance_block, &genesis_block, [0u8; 32]);

    patch_block(
        &mut transfer_block,
        &issuance_block,
        [
            0xb7, 0x76, 0x07, 0x73, 0x39, 0x2d, 0x36, 0x6e, 0x31, 0xc4, 0xb4, 0xce, 0xce, 0xfa,
            0x5e, 0x1f, 0x85, 0x60, 0x21, 0x21, 0x1e, 0x2f, 0x91, 0xa9, 0x1e, 0x05, 0xb3, 0x06,
            0xea, 0x80, 0x57, 0x24,
        ],
    );

    patch_block(
        &mut burn_block,
        &transfer_block,
        [
            0x6d, 0xec, 0x74, 0x8e, 0x07, 0xd1, 0x86, 0x67, 0xad, 0xb8, 0x39, 0x50, 0xe9, 0x48,
            0xc2, 0xca, 0x51, 0x9f, 0xad, 0x67, 0x45, 0x7a, 0xff, 0x4c, 0x3f, 0xa5, 0xad, 0x76,
            0xd2, 0x3a, 0xa8, 0x4c,
        ],
    );

    [
        genesis_block,
        Arc::new(issuance_block),
        Arc::new(transfer_block),
        Arc::new(burn_block),
    ]
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
