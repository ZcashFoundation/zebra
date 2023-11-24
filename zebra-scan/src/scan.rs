//! The scan task.

use std::time::Duration;

use color_eyre::{eyre::eyre, Report};
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};
use tracing::info;

use crate::storage::Storage;

type State = Buffer<
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>,
    zebra_state::Request,
>;

/// Wait a few seconds at startup so tip height is always `Some`.
const INITIAL_WAIT: Duration = Duration::from_secs(10);

/// The amount of time between checking and starting new scans.
const CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Start the scan task given state and storage.
///
/// - This function is dummy at the moment. It just makes sure we can read the storage and the state.
/// - Modificatiuons here might have an impact in the `scan_task_starts` test.
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
            zebra_state::Response::Tip(tip) => tip.unwrap().0,
            _ => unreachable!("unmatched response to a state::Tip request"),
        };

        // Read keys from the storage
        let available_keys = storage.get_sapling_keys();

        for key in available_keys {
            info!(
                "Scanning the blockchain for key {} from block 1 to {:?}",
                key.0, tip,
            );
        }

        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}
