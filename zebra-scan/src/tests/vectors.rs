//! Fixed integration test vectors for the scanner.

use std::sync::Arc;

use color_eyre::Result;

use sapling_crypto::{
    zip32::{DiversifiableFullViewingKey, ExtendedSpendingKey},
    Nullifier,
};
use zcash_client_backend::{
    encoding::{decode_extended_full_viewing_key, encode_extended_full_viewing_key},
    proto::compact_formats::ChainMetadata,
};
use zcash_primitives::constants::mainnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY;

use zebra_chain::{
    block::{Block, Height},
    chain_tip::ChainTip,
    parameters::Network,
    serialization::ZcashDeserializeInto,
};
use zebra_state::{SaplingScannedResult, TransactionIndex};

use crate::{
    scan::{block_to_compact, scan_block, scanning_keys},
    storage::db::tests::new_test_storage,
    tests::{fake_block, mock_sapling_efvk, ZECPAGES_SAPLING_VIEWING_KEY},
};

/// This test:
/// - Creates a viewing key and a fake block containing a Sapling output decryptable by the key.
/// - Scans the block.
/// - Checks that the result contains the txid of the tx containing the Sapling output.
#[tokio::test]
async fn scanning_from_fake_generated_blocks() -> Result<()> {
    let extsk = ExtendedSpendingKey::master(&[]);
    let dfvk: DiversifiableFullViewingKey = extsk.to_diversifiable_full_viewing_key();
    let nf = Nullifier([7; 32]);

    let (block, sapling_tree_size) = fake_block(1u32.into(), nf, &dfvk, 1, true, Some(0));

    assert_eq!(block.transactions.len(), 4);

    let scanning_keys = scanning_keys(&vec![dfvk]).expect("scanning key");

    let res = scan_block(&Network::Mainnet, &block, sapling_tree_size, &scanning_keys).unwrap();

    // The response should have one transaction relevant to the key we provided.
    assert_eq!(res.transactions().len(), 1);

    // Check that the original block contains the txid in the scanning result.
    assert!(block
        .transactions
        .iter()
        .map(|tx| tx.hash().bytes_in_display_order())
        .any(|txid| &txid == res.transactions()[0].txid().as_ref()));

    // Check that the txid in the scanning result matches the third tx in the original block.
    assert_eq!(
        res.transactions()[0].txid().as_ref(),
        &block.transactions[2].hash().bytes_in_display_order()
    );

    // The block hash of the response should be the same as the one provided.
    assert_eq!(res.block_hash().0, block.hash().0);

    Ok(())
}

/// Scan a populated state for the ZECpages viewing key.
/// This test is very similar to `scanning_from_populated_zebra_state` but with the ZECpages key.
/// There are no zechub transactions in the test data so we should get empty related transactions.
#[tokio::test]
async fn scanning_zecpages_from_populated_zebra_state() -> Result<()> {
    // Parse the key from ZECpages
    let efvk = decode_extended_full_viewing_key(
        HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY,
        ZECPAGES_SAPLING_VIEWING_KEY,
    )
    .unwrap();

    let dfvk = efvk.to_diversifiable_full_viewing_key();

    let network = Network::Mainnet;

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|block_bytes| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    // Create a populated state service.
    let (_state_service, read_only_state_service, latest_chain_tip, _chain_tip_change) =
        zebra_state::populated_state(blocks.clone(), &network).await;

    let db = read_only_state_service.db();

    // use the tip as starting height
    let mut height = latest_chain_tip.best_tip_height().unwrap();

    let mut transactions_found = 0;
    let mut transactions_scanned = 0;
    let mut blocks_scanned = 0;

    let scanning_keys = scanning_keys(&vec![dfvk]).expect("scanning key");

    while let Some(block) = db.block(height.into()) {
        // We use a dummy size of the Sapling note commitment tree. We can't set the size to zero
        // because the underlying scanning function would return
        // `zcash_client_backeng::scanning::ScanError::TreeSizeUnknown`.
        let sapling_commitment_tree_size = 1;

        let orchard_commitment_tree_size = 0;

        let chain_metadata = ChainMetadata {
            sapling_commitment_tree_size,
            orchard_commitment_tree_size,
        };

        let compact_block = block_to_compact(&block, chain_metadata);

        let res = scan_block(
            &network,
            &block,
            sapling_commitment_tree_size,
            &scanning_keys,
        )
        .expect("scanning block for the ZECpages viewing key should work");

        transactions_found += res.transactions().len();
        transactions_scanned += compact_block.vtx.len();
        blocks_scanned += 1;

        // scan backwards
        if height.is_min() {
            break;
        }
        height = height.previous()?;
    }

    // make sure all blocks and transactions were scanned
    assert_eq!(blocks_scanned, 11);
    assert_eq!(transactions_scanned, 11);

    // no relevant transactions should be found
    assert_eq!(transactions_found, 0);

    Ok(())
}

/// Creates a viewing key and a fake block containing a Sapling output decryptable by the key, scans
/// the block using the key, and adds the results to the database.
///
/// The purpose of this test is to check if our database and our scanning code are compatible.
#[test]
fn scanning_fake_blocks_store_key_and_results() -> Result<()> {
    let network = Network::Mainnet;

    // Generate a key
    let efvk = mock_sapling_efvk(&[]);
    let dfvk = efvk.to_diversifiable_full_viewing_key();
    let key_to_be_stored = encode_extended_full_viewing_key("zxviews", &efvk);

    // Create a database
    let mut storage = new_test_storage(&network);

    // Insert the generated key to the database
    storage.add_sapling_key(&key_to_be_stored, None);

    // Check key was added
    assert_eq!(storage.sapling_keys_last_heights().len(), 1);
    assert_eq!(
        storage
            .sapling_keys_last_heights()
            .get(&key_to_be_stored)
            .expect("height is stored")
            .next()
            .expect("height is not maximum"),
        network.sapling_activation_height()
    );

    let nf = Nullifier([7; 32]);

    let (block, sapling_tree_size) = fake_block(1u32.into(), nf, &dfvk, 1, true, Some(0));

    let scanning_keys = scanning_keys(&vec![dfvk]).expect("scanning key");

    let result = scan_block(&Network::Mainnet, &block, sapling_tree_size, &scanning_keys).unwrap();

    // The response should have one transaction relevant to the key we provided.
    assert_eq!(result.transactions().len(), 1);

    let result = SaplingScannedResult::from_bytes_in_display_order(
        *result.transactions()[0].txid().as_ref(),
    );

    // Add result to database
    storage.add_sapling_results(
        &key_to_be_stored,
        Height(1),
        [(TransactionIndex::from_usize(0), result)].into(),
    );

    // Check the result was added
    assert_eq!(
        storage.sapling_results(&key_to_be_stored).get(&Height(1)),
        Some(&vec![result])
    );

    Ok(())
}
