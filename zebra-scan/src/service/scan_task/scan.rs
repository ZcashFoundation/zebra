//! The scanner task and scanning APIs.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use color_eyre::{eyre::eyre, Report};
use itertools::Itertools;
use tokio::{
    sync::{mpsc::Sender, watch},
    task::JoinHandle,
};
use tower::{Service, ServiceExt};

use tracing::Instrument;
use zcash_address::unified::{Encoding, Fvk, Ufvk};
use zcash_client_backend::{
    data_api::ScannedBlock,
    encoding::decode_extended_full_viewing_key,
    keys::UnifiedFullViewingKey,
    proto::compact_formats::{
        ChainMetadata, CompactBlock, CompactSaplingOutput, CompactSaplingSpend, CompactTx,
    },
    scanning::{Nullifiers, ScanError, ScanningKeys},
};
use zcash_primitives::zip32::{AccountId, Scope};

use sapling_crypto::zip32::DiversifiableFullViewingKey;

use zebra_chain::{
    block::{Block, Height},
    chain_tip::ChainTip,
    diagnostic::task::WaitForPanics,
    parameters::Network,
    serialization::ZcashSerialize,
    transaction::Transaction,
};
use zebra_node_services::scan_service::response::ScanResult;
use zebra_state::{ChainTipChange, ReadStateService, SaplingScannedResult, TransactionIndex};

use crate::{
    service::{ScanTask, ScanTaskCommand},
    storage::{SaplingScanningKey, Storage},
};

use super::executor;

mod scan_range;

pub use scan_range::ScanRangeTaskBuilder;

/// The read state type used by the scanner.
pub type State = ReadStateService;

/// Wait a few seconds at startup for some blocks to get verified.
///
/// But sometimes the state might be empty if the network is slow.
const INITIAL_WAIT: Duration = Duration::from_secs(15);

/// The amount of time between checking for new blocks and starting new scans.
///
/// TODO: The current value is set to 10 so that tests don't sleep for too long and finish faster.
///       Set it to 30 after #8250 gets addressed or remove this const completely in the refactor.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// We log an info log with progress after this many blocks.
const INFO_LOG_INTERVAL: u32 = 10_000;

/// Start a scan task that reads blocks from `state`, scans them with the configured keys in
/// `storage`, and then writes the results to `storage`.
pub async fn start(
    state: State,
    chain_tip_change: ChainTipChange,
    storage: Storage,
    mut cmd_receiver: tokio::sync::mpsc::Receiver<ScanTaskCommand>,
) -> Result<(), Report> {
    let network = storage.network();
    let sapling_activation_height = network.sapling_activation_height();

    info!(?network, "starting scan task");

    // Do not scan and notify if we are below sapling activation height.
    #[cfg(not(test))]
    wait_for_height(
        sapling_activation_height,
        "Sapling activation",
        state.clone(),
    )
    .await?;

    // Read keys from the storage on disk, which can block async execution.
    let key_storage = storage.clone();
    let key_heights = tokio::task::spawn_blocking(move || key_storage.sapling_keys_last_heights())
        .wait_for_panics()
        .await;
    let key_heights = Arc::new(key_heights);

    let mut height = get_min_height(&key_heights).unwrap_or(sapling_activation_height);

    info!(start_height = ?height, "got min scan height");

    // Parse and convert keys once, then use them to scan all blocks.
    // There is some cryptography here, but it should be fast even with thousands of keys.
    let mut parsed_keys: HashMap<SaplingScanningKey, DiversifiableFullViewingKey> = key_heights
        .keys()
        .map(|key| Ok::<_, Report>((key.clone(), sapling_key_to_dfvk(key, &network)?)))
        .try_collect()?;

    let mut subscribed_keys: HashMap<SaplingScanningKey, Sender<ScanResult>> = HashMap::new();

    let (subscribed_keys_sender, subscribed_keys_receiver) =
        tokio::sync::watch::channel(Arc::new(subscribed_keys.clone()));

    let (scan_task_sender, scan_task_executor_handle) =
        executor::spawn_init(subscribed_keys_receiver.clone());
    let mut scan_task_executor_handle = Some(scan_task_executor_handle);

    // Give empty states time to verify some blocks before we start scanning.
    tokio::time::sleep(INITIAL_WAIT).await;

    loop {
        if let Some(handle) = scan_task_executor_handle {
            if handle.is_finished() {
                warn!("scan task finished unexpectedly");

                handle.await?.map_err(|err| eyre!(err))?;
                return Ok(());
            } else {
                scan_task_executor_handle = Some(handle);
            }
        }

        let was_parsed_keys_empty = parsed_keys.is_empty();

        let (new_keys, new_result_senders, new_result_receivers) =
            ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, &network)?;

        subscribed_keys.extend(new_result_senders);
        // Drop any results senders that are closed from subscribed_keys
        subscribed_keys.retain(|key, sender| !sender.is_closed() && parsed_keys.contains_key(key));

        // Send the latest version of `subscribed_keys` before spawning the scan range task
        subscribed_keys_sender
            .send(Arc::new(subscribed_keys.clone()))
            .expect("last receiver should not be dropped while this task is running");

        for (result_receiver, rsp_tx) in new_result_receivers {
            // Ignore send errors, we drop any closed results channels above.
            let _ = rsp_tx.send(result_receiver);
        }

        if !new_keys.is_empty() {
            let state = state.clone();
            let storage = storage.clone();

            let start_height = new_keys
                .iter()
                .map(|(_, (_, height))| *height)
                .min()
                .unwrap_or(sapling_activation_height);

            if was_parsed_keys_empty {
                info!(?start_height, "setting new start height");
                height = start_height;
            }
            // Skip spawning ScanRange task if `start_height` is at or above the current height
            else if start_height < height {
                scan_task_sender
                    .send(ScanRangeTaskBuilder::new(height, new_keys, state, storage))
                    .await
                    .expect("scan_until_task channel should not be closed");
            }
        }

        if !parsed_keys.is_empty() {
            let scanned_height = scan_height_and_store_results(
                height,
                state.clone(),
                Some(chain_tip_change.clone()),
                storage.clone(),
                key_heights.clone(),
                parsed_keys.clone(),
                subscribed_keys_receiver.clone(),
            )
            .await?;

            // If we've reached the tip, sleep for a while then try and get the same block.
            if scanned_height.is_none() {
                tokio::time::sleep(CHECK_INTERVAL).await;
                continue;
            }
        } else {
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        }

        height = height
            .next()
            .expect("a valid blockchain never reaches the max height");
    }
}

/// Polls state service for tip height every [`CHECK_INTERVAL`] until the tip reaches the provided `tip_height`
pub async fn wait_for_height(
    height: Height,
    height_name: &'static str,
    state: State,
) -> Result<(), Report> {
    loop {
        let tip_height = tip_height(state.clone()).await?;
        if tip_height < height {
            info!(
                "scanner is waiting for {height_name}. Current tip: {}, {height_name}: {}",
                tip_height.0, height.0
            );

            tokio::time::sleep(CHECK_INTERVAL).await;
        } else {
            info!(
                "scanner finished waiting for {height_name}. Current tip: {}, {height_name}: {}",
                tip_height.0, height.0
            );

            break;
        }
    }

    Ok(())
}

/// Get the block at `height` from `state`, scan it with the keys in `parsed_keys`, and store the
/// results in `storage`. If `height` is lower than the `key_birthdays` for that key, skip it.
///
/// Returns:
/// - `Ok(Some(height))` if the height was scanned,
/// - `Ok(None)` if the height was not in the state, and
/// - `Err(error)` on fatal errors.
pub async fn scan_height_and_store_results(
    height: Height,
    mut state: State,
    chain_tip_change: Option<ChainTipChange>,
    storage: Storage,
    key_last_scanned_heights: Arc<HashMap<SaplingScanningKey, Height>>,
    parsed_keys: HashMap<String, DiversifiableFullViewingKey>,
    subscribed_keys_receiver: watch::Receiver<Arc<HashMap<String, Sender<ScanResult>>>>,
) -> Result<Option<Height>, Report> {
    let network = storage.network();

    // Only log at info level every 100,000 blocks.
    //
    // TODO: also log progress every 5 minutes once we reach the tip?
    let is_info_log = height.0 % INFO_LOG_INTERVAL == 0;

    // Get a block from the state.
    // We can't use ServiceExt::oneshot() here, because it causes lifetime errors in init().
    let block = state
        .ready()
        .await
        .map_err(|e| eyre!(e))?
        .call(zebra_state::ReadRequest::Block(height.into()))
        .await
        .map_err(|e| eyre!(e))?;

    let block = match block {
        zebra_state::ReadResponse::Block(Some(block)) => block,
        zebra_state::ReadResponse::Block(None) => return Ok(None),
        _ => unreachable!("unmatched response to a state::Block request"),
    };

    for (key_index_in_task, (sapling_key, _)) in parsed_keys.iter().enumerate() {
        match key_last_scanned_heights.get(sapling_key) {
            // Only scan what was not scanned for each key
            Some(last_scanned_height) if height <= *last_scanned_height => continue,

            Some(last_scanned_height) if is_info_log => {
                if let Some(chain_tip_change) = &chain_tip_change {
                    // # Security
                    //
                    // We can't log `sapling_key` here because it is a private viewing key. Anyone who reads
                    // the logs could use the key to view those transactions.
                    info!(
                        "Scanning the blockchain for key {}, started at block {:?}, now at block {:?}, current tip {:?}",
                        key_index_in_task, last_scanned_height.next().expect("height is not maximum").as_usize(),
                        height.as_usize(),
                        chain_tip_change.latest_chain_tip().best_tip_height().expect("we should have a tip to scan").as_usize(),
                    );
                } else {
                    info!(
                        "Scanning the blockchain for key {}, started at block {:?}, now at block {:?}",
                        key_index_in_task, last_scanned_height.next().expect("height is not maximum").as_usize(),
                        height.as_usize(),
                    );
                }
            }

            _other => {}
        };

        let subscribed_keys_receiver = subscribed_keys_receiver.clone();

        let sapling_key = sapling_key.clone();
        let block = block.clone();
        let mut storage = storage.clone();
        let network = network.clone();
        let parsed_keys = parsed_keys.clone();

        // We use a dummy size of the Sapling note commitment tree.
        //
        // We can't set the size to zero, because the underlying scanning function would return
        // `zcash_client_backeng::scanning::ScanError::TreeSizeUnknown`.
        //
        // And we can't set them close to 0, because the scanner subtracts the number of notes
        // in the block, and panics with "attempt to subtract with overflow". The number of
        // notes in a block must be less than this value, this is a consensus rule.
        //
        // TODO: use the real sapling tree size: `zs::Response::SaplingTree().position() + 1`
        let sapling_tree_size = 1 << 16;

        tokio::task::spawn_blocking(move || {
            // TODO:
            // - Wait until https://github.com/zcash/librustzcash/pull/1400 makes it to a release.
            // - Create the scanning keys outside of this thread and move them here instead.
            let scanning_keys = scanning_keys(parsed_keys.values()).expect("scanning keys");

            let scanned_block = scan_block(&network, &block, sapling_tree_size, &scanning_keys)
                .map_err(|e| eyre!(e))?;

            let scanning_result = scanned_block_to_db_result(scanned_block);

            let latest_subscribed_keys = subscribed_keys_receiver.borrow().clone();
            if let Some(results_sender) = latest_subscribed_keys.get(&sapling_key).cloned() {
                for (_tx_index, tx_id) in scanning_result.clone() {
                    // TODO: Handle `SendErrors` by dropping sender from `subscribed_keys`
                    let _ = results_sender.try_send(ScanResult {
                        key: sapling_key.clone(),
                        height,
                        tx_id: tx_id.into(),
                    });
                }
            }

            storage.add_sapling_results(&sapling_key, height, scanning_result);
            Ok::<_, Report>(())
        })
        .wait_for_panics()
        .await?;
    }

    Ok(Some(height))
}

/// Returns the transactions from `block` belonging to the given `scanning_keys`.
///
/// # Performance / Hangs
///
/// This method can block while reading database files, so it must be inside spawn_blocking()
/// in async code.
///
/// TODO:
/// - Pass the real `sapling_tree_size` parameter from the state.
/// - Add other prior block metadata.
pub fn scan_block(
    network: &Network,
    block: &Block,
    sapling_tree_size: u32,
    scanning_key: &ScanningKeys<AccountId, (AccountId, Scope)>,
) -> Result<ScannedBlock<AccountId>, ScanError> {
    // TODO: Implement a check that returns early when the block height is below the Sapling
    // activation height.

    let chain_metadata = ChainMetadata {
        sapling_commitment_tree_size: sapling_tree_size,
        // Orchard is not supported at the moment so the tree size can be 0.
        orchard_commitment_tree_size: 0,
    };

    zcash_client_backend::scanning::scan_block(
        &zp_network(network),
        block_to_compact(block, chain_metadata),
        scanning_key,
        // Ignore whether notes are change from a viewer's own spends for now.
        &Nullifiers::empty(),
        // Ignore previous blocks for now.
        None,
    )
}

/// Converts a Zebra-format scanning key into diversifiable full viewing key.
// TODO: use `ViewingKey::parse` from zebra-chain instead
pub fn sapling_key_to_dfvk(
    key: &SaplingScanningKey,
    network: &Network,
) -> Result<DiversifiableFullViewingKey, Report> {
    Ok(
        decode_extended_full_viewing_key(network.sapling_efvk_hrp(), key)
            .map_err(|e| eyre!(e))?
            .to_diversifiable_full_viewing_key(),
    )
}

/// Converts a zebra block and meta data into a compact block.
pub fn block_to_compact(block: &Block, chain_metadata: ChainMetadata) -> CompactBlock {
    CompactBlock {
        height: block
            .coinbase_height()
            .expect("verified block should have a valid height")
            .0
            .into(),
        // TODO: performance: look up the block hash from the state rather than recalculating it
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
        // TODO: performance: look up the tx hash from the state rather than recalculating it
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
fn scanned_block_to_db_result<Nf>(
    scanned_block: ScannedBlock<Nf>,
) -> BTreeMap<TransactionIndex, SaplingScannedResult> {
    scanned_block
        .transactions()
        .iter()
        .map(|tx| {
            (
                TransactionIndex::from_usize(tx.block_index()),
                SaplingScannedResult::from_bytes_in_display_order(*tx.txid().as_ref()),
            )
        })
        .collect()
}

/// Get the minimal height available in a key_heights map.
fn get_min_height(map: &HashMap<String, Height>) -> Option<Height> {
    map.values().cloned().min()
}

/// Get tip height or return genesis block height if no tip is available.
async fn tip_height(mut state: State) -> Result<Height, Report> {
    let tip = state
        .ready()
        .await
        .map_err(|e| eyre!(e))?
        .call(zebra_state::ReadRequest::Tip)
        .await
        .map_err(|e| eyre!(e))?;

    match tip {
        zebra_state::ReadResponse::Tip(Some((height, _hash))) => Ok(height),
        zebra_state::ReadResponse::Tip(None) => Ok(Height(0)),
        _ => unreachable!("unmatched response to a state::Tip request"),
    }
}

/// Initialize the scanner based on its config, and spawn a task for it.
///
/// TODO: add a test for this function.
pub fn spawn_init(
    storage: Storage,
    state: State,
    chain_tip_change: ChainTipChange,
    cmd_receiver: tokio::sync::mpsc::Receiver<ScanTaskCommand>,
) -> JoinHandle<Result<(), Report>> {
    tokio::spawn(start(state, chain_tip_change, storage, cmd_receiver).in_current_span())
}

/// Turns an iterator of [`DiversifiableFullViewingKey`]s to [`ScanningKeys`].
pub fn scanning_keys<'a>(
    dfvks: impl IntoIterator<Item = &'a DiversifiableFullViewingKey>,
) -> Result<ScanningKeys<AccountId, (AccountId, Scope)>, Report> {
    dfvks
        .into_iter()
        .enumerate()
        .map(|(i, dfvk)| {
            let account = AccountId::try_from(u32::try_from(i)?)
                .map_err(|e| eyre!("Invalid AccountId: {:?}", e))?;
            Ok((account, dfvk_to_ufvk(dfvk)?))
        })
        .try_collect::<(_, _), Vec<(_, _)>, _>()
        .map(ScanningKeys::from_account_ufvks)
}

/// Turns a [`DiversifiableFullViewingKey`] to [`UnifiedFullViewingKey`].
pub fn dfvk_to_ufvk(dfvk: &DiversifiableFullViewingKey) -> Result<UnifiedFullViewingKey, Report> {
    UnifiedFullViewingKey::parse(&Ufvk::try_from_items(vec![Fvk::try_from((
        2,
        &dfvk.to_bytes()[..],
    ))?])?)
    .map_err(|e| eyre!(e))
}

/// Returns the [`zcash_primitives::consensus::Network`] for this network.
pub fn zp_network(network: &Network) -> zcash_primitives::consensus::Network {
    match network {
        Network::Mainnet => zcash_primitives::consensus::Network::MainNetwork,
        Network::Testnet(_) => zcash_primitives::consensus::Network::TestNetwork,
    }
}
