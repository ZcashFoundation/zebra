//! Types and method implementations for [`ScanTask`]

use std::sync::{mpsc, Arc};

use color_eyre::Report;
use tokio::task::JoinHandle;

use zebra_chain::parameters::Network;
use zebra_state::ChainTipChange;

use crate::Config;

mod commands;
pub mod scan;

pub use commands::*;

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
    pub fn spawn(
        config: &Config,
        network: Network,
        state: scan::State,
        chain_tip_change: ChainTipChange,
    ) -> Self {
        let (cmd_sender, cmd_receiver) = mpsc::channel();

        Self {
            handle: Arc::new(scan::spawn_init(
                config,
                network,
                state,
                chain_tip_change,
                cmd_receiver,
            )),
            cmd_sender,
        }
    }
}
