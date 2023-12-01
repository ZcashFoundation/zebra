//! The scanner task and scanning APIs.

use std::{collections::HashMap, sync::Arc, time::Duration};

use color_eyre::{eyre::eyre, Report};
use itertools::Itertools;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};
use tracing::info;

use zcash_client_backend::{
    data_api::ScannedBlock,
    encoding::decode_extended_full_viewing_key,
    proto::compact_formats::{
        ChainMetadata, CompactBlock, CompactSaplingOutput, CompactSaplingSpend, CompactTx,
    },
    scanning::{ScanError, ScanningKey},
};
use zcash_primitives::{
    constants::*,
    sapling::SaplingIvk,
    zip32::{AccountId, DiversifiableFullViewingKey, Scope},
};

use zebra_chain::{
    block::Block,
    chain_tip::ChainTip,
    diagnostic::task::WaitForPanics,
    parameters::Network,
    serialization::ZcashSerialize,
    transaction::{self, Transaction},
};
use zebra_state::{ChainTipChange, SaplingScannedResult};

use crate::storage::{SaplingScanningKey, Storage};

/// The generic state type used by the scanner.
pub type State = Buffer<
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>,
    zebra_state::Request,
>;

/// Wait a few seconds at startup so tip height is always `Some`.
const INITIAL_WAIT: Duration = Duration::from_secs(10);

/// The amount of time between checking and starting new scans.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Start the scan task given state and storage.
///
/// - This function is dummy at the moment. It just makes sure we can read the storage and the state.
/// - Modifications here might have an impact in the `scan_task_starts` test.
/// - Real scanning code functionality will be added in the future here.
pub async fn start(mut state: State, storage: Storage) -> Result<(), Report> {
    // We want to make sure the state has a tip height available before we start scanning.
    tokio::time::sleep(INITIAL_WAIT).await;

    loop {
        // Make sure we can query the state
        let request = state
            .ready()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::Tip)
            .await
            .map_err(|e| eyre!(e));

        let tip = match request? {
            zebra_state::Response::Tip(tip) => tip,
            _ => unreachable!("unmatched response to a state::Tip request"),
        };

        // Read keys from the storage on disk, which can block.
        let key_storage = storage.clone();
        let available_keys = tokio::task::spawn_blocking(move || key_storage.sapling_keys())
            .wait_for_panics()
            .await;

        for key in available_keys {
            info!(
                "Scanning the blockchain for key {} from block {:?} to {:?}",
                key.0, key.1, tip,
            );
        }

        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

/// Returns transactions belonging to the given `ScanningKey`.
///
/// # Performance / Hangs
///
/// This method can block while reading database files, so it must be inside spawn_blocking()
/// in async code.
///
/// TODO:
/// - Remove the `sapling_tree_size` parameter or turn it into an `Option` once we have access to
/// Zebra's state, and we can retrieve the tree size ourselves.
/// - Add prior block metadata once we have access to Zebra's state.
pub fn scan_block<K: ScanningKey>(
    network: Network,
    block: Arc<Block>,
    sapling_tree_size: u32,
    scanning_key: &K,
) -> Result<ScannedBlock<K::Nf>, ScanError> {
    // TODO: Implement a check that returns early when the block height is below the Sapling
    // activation height.

    let network: zcash_primitives::consensus::Network = network.into();

    let chain_metadata = ChainMetadata {
        sapling_commitment_tree_size: sapling_tree_size,
        // Orchard is not supported at the moment so the tree size can be 0.
        orchard_commitment_tree_size: 0,
    };

    // Use a dummy `AccountId` as we don't use accounts yet.
    let dummy_account = AccountId::from(0);

    // We only support scanning one key and one block per function call for now.
    let scanning_keys = vec![(&dummy_account, scanning_key)];

    zcash_client_backend::scanning::scan_block(
        &network,
        block_to_compact(block, chain_metadata),
        &scanning_keys,
        // Ignore whether notes are change from a viewer's own spends for now.
        &[],
        // Ignore previous blocks for now.
        None,
    )
}

/// Converts a Zebra-format scanning key into some `scan_block()` keys.
///
/// Currently only accepts extended full viewing keys, and returns both their diversifiable full
/// viewing key and their individual viewing key, for testing purposes.
///
/// TODO: work out what string format is used for SaplingIvk, if any, and support it here
///       performance: stop returning both the dfvk and ivk for the same key
pub fn sapling_key_to_scan_block_keys(
    sapling_key: &SaplingScanningKey,
    network: Network,
) -> Result<(Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>), Report> {
    let hrp = if network.is_a_test_network() {
        // Assume custom testnets have the same HRP
        //
        // TODO: add the regtest HRP here
        testnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
    } else {
        mainnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
    };

    let efvk = decode_extended_full_viewing_key(hrp, sapling_key).map_err(|e| eyre!(e))?;

    // Just return all the keys for now, so we can be sure our code supports them.
    let dfvk = efvk.to_diversifiable_full_viewing_key();
    let eivk = dfvk.to_ivk(Scope::External);
    let iivk = dfvk.to_ivk(Scope::Internal);

    Ok((vec![dfvk], vec![eivk, iivk]))
}

/// Converts a zebra block and meta data into a compact block.
pub fn block_to_compact(block: Arc<Block>, chain_metadata: ChainMetadata) -> CompactBlock {
    CompactBlock {
        height: block
            .coinbase_height()
            .expect("verified block should have a valid height")
            .0
            .into(),
        hash: block.hash().bytes_in_display_order().to_vec(),
        prev_hash: block
            .header
            .previous_block_hash
            .bytes_in_display_order()
            .to_vec(),
        time: block
            .header
            .time
            .timestamp()
            .try_into()
            .expect("unsigned 32-bit times should work until 2105"),
        header: block
            .header
            .zcash_serialize_to_vec()
            .expect("verified block should serialize"),
        vtx: block
            .transactions
            .iter()
            .cloned()
            .enumerate()
            .map(transaction_to_compact)
            .collect(),
        chain_metadata: Some(chain_metadata),

        // The protocol version is used for the gRPC wire format, so it isn't needed here.
        proto_version: 0,
    }
}

/// Converts a zebra transaction into a compact transaction.
fn transaction_to_compact((index, tx): (usize, Arc<Transaction>)) -> CompactTx {
    CompactTx {
        index: index
            .try_into()
            .expect("tx index in block should fit in u64"),
        hash: tx.hash().bytes_in_display_order().to_vec(),

        // `fee` is not checked by the `scan_block` function. It is allowed to be unset.
        // <https://docs.rs/zcash_client_backend/latest/zcash_client_backend/proto/compact_formats/struct.CompactTx.html#structfield.fee>
        fee: 0,

        spends: tx
            .sapling_nullifiers()
            .map(|nf| CompactSaplingSpend {
                nf: <[u8; 32]>::from(*nf).to_vec(),
            })
            .collect(),

        // > output encodes the cmu field, ephemeralKey field, and a 52-byte prefix of the encCiphertext field of a Sapling Output
        //
        // <https://docs.rs/zcash_client_backend/latest/zcash_client_backend/proto/compact_formats/struct.CompactSaplingOutput.html>
        outputs: tx
            .sapling_outputs()
            .map(|output| CompactSaplingOutput {
                cmu: output.cm_u.to_bytes().to_vec(),
                ephemeral_key: output
                    .ephemeral_key
                    .zcash_serialize_to_vec()
                    .expect("verified output should serialize successfully"),
                ciphertext: output
                    .enc_ciphertext
                    .zcash_serialize_to_vec()
                    .expect("verified output should serialize successfully")
                    .into_iter()
                    .take(52)
                    .collect(),
            })
            .collect(),

        // `actions` is not checked by the `scan_block` function.
        actions: vec![],
    }
}

/// Convert a scanned block to a list of scanner database results.
fn scanned_block_to_db_result<Nf>(scanned_block: ScannedBlock<Nf>) -> Vec<SaplingScannedResult> {
    scanned_block
        .transactions()
        .iter()
        .map(|tx| transaction::Hash::from_bytes_in_display_order(tx.txid.as_ref()))
        .collect()
}
