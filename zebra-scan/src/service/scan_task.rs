//! Types and method implementations for [`ScanTask`]

use std::sync::{mpsc, Arc};

use color_eyre::Report;
use tokio::task::JoinHandle;

use zebra_state::ChainTipChange;

use crate::storage::Storage;

mod commands;
mod executor;
pub mod scan;

pub use commands::ScanTaskCommand;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

#[derive(Debug, Clone)]
/// Scan task handle and command channel sender
pub struct ScanTask {
    /// [`JoinHandle`] of scan task
    pub handle: Arc<JoinHandle<Result<(), Report>>>,

    /// Task command channel sender
    pub cmd_sender: mpsc::Sender<ScanTaskCommand>,
}

impl ScanTask {
    /// Spawns a new [`ScanTask`].
    pub fn spawn(db: Storage, state: scan::State, chain_tip_change: ChainTipChange) -> Self {
        // TODO: Use a bounded channel or move this logic to the scan service or another service.
        let (cmd_sender, cmd_receiver) = mpsc::channel();

        Self {
            handle: Arc::new(scan::spawn_init(db, state, chain_tip_change, cmd_receiver)),
            cmd_sender,
        }
    }
}
