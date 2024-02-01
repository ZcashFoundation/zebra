//! Tests for the scan task.

use std::sync::{
    mpsc::{self, Receiver},
    Arc,
};

use super::{ScanTask, ScanTaskCommand};

impl ScanTask {
    /// Spawns a new [`ScanTask`] for tests.
    pub fn mock() -> (Self, Receiver<ScanTaskCommand>) {
        let (cmd_sender, cmd_receiver) = mpsc::channel();

        (
            Self {
                handle: Arc::new(tokio::spawn(std::future::pending())),
                cmd_sender,
            },
            cmd_receiver,
        )
    }
}
