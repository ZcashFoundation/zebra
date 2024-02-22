//! Tests for the scan task.

use std::sync::Arc;

use super::{ScanTask, ScanTaskCommand, SCAN_TASK_BUFFER_SIZE};

#[cfg(test)]
mod vectors;

impl ScanTask {
    /// Spawns a new [`ScanTask`] for tests.
    pub fn mock() -> (Self, tokio::sync::mpsc::Receiver<ScanTaskCommand>) {
        let (cmd_sender, cmd_receiver) = tokio::sync::mpsc::channel(SCAN_TASK_BUFFER_SIZE);

        (
            Self {
                handle: Arc::new(tokio::spawn(std::future::pending())),
                cmd_sender,
            },
            cmd_receiver,
        )
    }
}
