use zebra_chain::orchard_zsa::tests::vectors::BLOCKS;

use std::sync::Arc;

use color_eyre::eyre::Report;

use zebra_chain::{
    block::{genesis::regtest_genesis_block, Block, Height},
    parameters::Network,
    serialization::ZcashDeserialize,
};

use zebra_state::{Config, Request, Response};

use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

fn patch_block_coinbase_height(block: &mut Block, previous_block: &Block) {
    let mut transactions: Vec<Arc<zebra_chain::transaction::Transaction>> =
        block.transactions.iter().cloned().collect();
    //let transactions: &mut Vec<Arc<zebra_chain::transaction::Transaction>> =
    //    Arc::make_mut(&mut block.transactions);
    if let Some(tx_arc) = transactions.first_mut() {
        let tx = Arc::make_mut(&mut *tx_arc);
        if let Some(input) = tx.inputs_mut().first_mut() {
            if let zebra_chain::transparent::Input::Coinbase { ref mut height, .. } = input {
                *height = previous_block
                    .coinbase_height()
                    .expect("block has coinbase_height")
                    .next()
                    .expect("block has next coinbase_height"); //zebra_chain::block::Height(new_height)
            }
        }
    }
    block.transactions = transactions.into();
}

fn patch_block(block: &mut Block, previous_block: &Block, commitment_bytes: [u8; 32]) {
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = previous_block.hash();
    *header.commitment_bytes = commitment_bytes;
    patch_block_coinbase_height(block, previous_block);
}

// FIXME: remove printlns
fn create_transcript_data(
    network: Network,
) -> Vec<(Request, Result<Response, ExpectedTranscriptError>)> {
    let genesis_block = regtest_genesis_block();

    let mut issuance_block: Block =
        Block::zcash_deserialize(BLOCKS[1].as_ref()).expect("issuance block should deserialize");
    let mut transfer_block: Block =
        Block::zcash_deserialize(BLOCKS[2].as_ref()).expect("transfer block should deserialize");
    let mut burn_block: Block =
        Block::zcash_deserialize(BLOCKS[3].as_ref()).expect("burn block should deserialize");

    let genesis_hash = genesis_block.hash();
    println!(
        "Genesis hash: {:?}, previous: {:?}",
        genesis_hash, genesis_block.header.previous_block_hash
    );

    //assert_eq!(genesis_hash, network.genesis_hash());

    // Patch issuance block
    patch_block(&mut issuance_block, &genesis_block, [0u8; 32]);

    let issuance_hash = issuance_block.hash();
    println!(
        "Issuance hash: {:?}, previous: {:?}",
        issuance_hash, issuance_block.header.previous_block_hash
    );

    // Patch transfer block
    patch_block(
        &mut transfer_block,
        &issuance_block,
        [
            0xb7, 0x76, 0x07, 0x73, 0x39, 0x2d, 0x36, 0x6e, 0x31, 0xc4, 0xb4, 0xce, 0xce, 0xfa,
            0x5e, 0x1f, 0x85, 0x60, 0x21, 0x21, 0x1e, 0x2f, 0x91, 0xa9, 0x1e, 0x05, 0xb3, 0x06,
            0xea, 0x80, 0x57, 0x24,
        ],
    );

    let transfer_hash = transfer_block.hash();
    println!(
        "Transfer hash: {:?}, previous: {:?}",
        transfer_hash, transfer_block.header.previous_block_hash
    );

    // Patch burn block
    patch_block(
        &mut burn_block,
        &transfer_block,
        [
            0x6d, 0xec, 0x74, 0x8e, 0x07, 0xd1, 0x86, 0x67, 0xad, 0xb8, 0x39, 0x50, 0xe9, 0x48,
            0xc2, 0xca, 0x51, 0x9f, 0xad, 0x67, 0x45, 0x7a, 0xff, 0x4c, 0x3f, 0xa5, 0xad, 0x76,
            0xd2, 0x3a, 0xa8, 0x4c,
        ],
    );

    let burn_hash = burn_block.hash();
    println!(
        "Burn hash: {:?}, previous: {:?}",
        burn_hash, burn_block.header.previous_block_hash
    );

    //println!("{:#?}", transfer_block);

    vec![
        (
            Request::CommitCheckpointVerifiedBlock(genesis_block.clone().into()),
            Ok(Response::Committed(genesis_hash)),
        ),
        (
            Request::Block(genesis_hash.into()),
            Ok(Response::Block(Some(genesis_block))),
        ),
        (
            //Request::CommitSemanticallyVerifiedBlock(Arc::new(issuance_block.clone()).into()),
            Request::CommitCheckpointVerifiedBlock(Arc::new(issuance_block.clone()).into()),
            Ok(Response::Committed(issuance_hash)),
        ),
        (
            Request::Block(issuance_hash.into()),
            Ok(Response::Block(Some(Arc::new(issuance_block)))),
        ),
        (
            Request::CommitCheckpointVerifiedBlock(Arc::new(transfer_block.clone()).into()),
            Ok(Response::Committed(transfer_hash)),
        ),
        (
            Request::Block(transfer_hash.into()),
            Ok(Response::Block(Some(Arc::new(transfer_block)))),
        ),
        (
            Request::CommitCheckpointVerifiedBlock(Arc::new(burn_block.clone()).into()),
            Ok(Response::Committed(burn_hash)),
        ),
        (
            Request::Block(burn_hash.into()),
            Ok(Response::Block(Some(Arc::new(burn_block)))),
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
//#[tokio::test]
async fn check_zsa_workflow() -> Result<(), Report> {
    //let _init_guard = zebra_test::init();
    //let network = Network::new_default_testnet();
    let network = Network::new_regtest(None, None, None);

    for transcript_data in &[create_transcript_data(network.clone())] {
        // We're not verifying UTXOs here.
        let (service, _, _, _) =
            zebra_state::init(Config::ephemeral(), &network.clone(), Height::MAX, 0);
        let transcript = Transcript::from(transcript_data.iter().cloned());
        // check the on disk service against the transcript
        transcript.check(service).await?;
    }

    Ok(())
}
