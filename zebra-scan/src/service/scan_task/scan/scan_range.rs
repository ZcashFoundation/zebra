//! Functions for registering new keys in the scan task

use std::{collections::HashMap, sync::Arc};

use tokio::task::JoinHandle;
use tracing::Instrument;
use zcash_primitives::{sapling::SaplingIvk, zip32::DiversifiableFullViewingKey};
use zebra_chain::{block::Height, BoxError};
use zebra_state::SaplingScanningKey;

use crate::{
    scan::{scan_height_and_store_results, wait_for_height, State, CHECK_INTERVAL},
    storage::Storage,
};

/// A builder for a scan until task
pub struct ScanRangeTaskBuilder {
    /// The range of block heights that should be scanned for these keys
    // TODO: Remove start heights from keys and require that all keys per task use the same start height
    height_range: std::ops::Range<Height>,

    /// The keys to be used for scanning blocks in this task
    keys: HashMap<SaplingScanningKey, (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>, Height)>,

    /// A handle to the state service for reading the blocks and the chain tip height
    state: State,

    /// A handle to the zebra-scan database for storing results
    storage: Storage,
}

impl ScanRangeTaskBuilder {
    /// Creates a new [`ScanRangeTaskBuilder`]
    pub fn new(
        stop_height: Height,
        keys: HashMap<
            SaplingScanningKey,
            (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>, Height),
        >,
        state: State,
        storage: Storage,
    ) -> Self {
        Self {
            height_range: Height::MIN..stop_height,
            keys,
            state,
            storage,
        }
    }

    /// Spawns a `scan_range()` task and returns its [`JoinHandle`]
    // TODO: return a tuple with a shutdown sender
    pub fn spawn(self) -> JoinHandle<Result<(), BoxError>> {
        let Self {
            height_range,
            keys,
            state,
            storage,
        } = self;

        tokio::spawn(scan_range(height_range.end, keys, state, storage).in_current_span())
    }
}

/// Start a scan task that reads blocks from `state` within the provided height range,
/// scans them with the configured keys in `storage`, and then writes the results to `storage`.
// TODO: update the first parameter to `std::ops::Range<Height>`
pub async fn scan_range(
    stop_before_height: Height,
    keys: HashMap<SaplingScanningKey, (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>, Height)>,
    state: State,
    storage: Storage,
) -> Result<(), BoxError> {
    let sapling_activation_height = storage.min_sapling_birthday_height();
    // Do not scan and notify if we are below sapling activation height.
    wait_for_height(
        sapling_activation_height,
        "Sapling activation",
        state.clone(),
    )
    .await?;

    let key_heights: HashMap<String, Height> = keys
        .iter()
        .map(|(key, (_, _, height))| (key.clone(), *height))
        .collect();
    let key_heights = Arc::new(key_heights);

    let mut height = key_heights
        .values()
        .cloned()
        .min()
        .unwrap_or(sapling_activation_height);

    // Parse and convert keys once, then use them to scan all blocks.
    let parsed_keys: HashMap<
        SaplingScanningKey,
        (Vec<DiversifiableFullViewingKey>, Vec<SaplingIvk>),
    > = keys
        .into_iter()
        .map(|(key, (decoded_dfvks, decoded_ivks, _h))| (key, (decoded_dfvks, decoded_ivks)))
        .collect();

    while height < stop_before_height {
        let scanned_height = scan_height_and_store_results(
            height,
            state.clone(),
            None,
            storage.clone(),
            key_heights.clone(),
            parsed_keys.clone(),
        )
        .await?;

        // If we've reached the tip, sleep for a while then try and get the same block.
        if scanned_height.is_none() {
            tokio::time::sleep(CHECK_INTERVAL).await;
            continue;
        }

        height = height
            .next()
            .expect("a valid blockchain never reaches the max height");
    }

    Ok(())
}
