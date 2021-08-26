use tokio::sync::watch;

/// A helper type to determine if the mempool is enabled or not.
///
/// This type can be used as a handle, so cloning it is cheap.
#[derive(Clone, Debug)]
pub struct MempoolStatus {
    latest_sync_length: watch::Receiver<Vec<usize>>,
}

impl MempoolStatus {
    /// Create an instance of [`MempoolStatus`].
    ///
    /// The status is determined based on the latest counts of synchronized blocks, observed
    /// through `latest_sync_length`.
    pub fn new(latest_sync_length: watch::Receiver<Vec<usize>>) -> Self {
        MempoolStatus { latest_sync_length }
    }

    /// Wait until the mempool is enabled.
    ///
    /// Returns an error if there's no way to know if the mempool status has changed
    pub async fn wait_until_enabled(&mut self) -> Result<(), watch::error::RecvError> {
        while !self.is_enabled() {
            let wait_result = self.latest_sync_length.changed().await;

            if let Err(error) = wait_result {
                // If this happens, it's likely that Zebra is shutting down.
                debug!("Mempool crawler stopped receiving latest sync lengths. Stopping...");
                return Err(error);
            }
        }

        Ok(())
    }

    /// Check if the mempool is currently enabled.
    pub fn is_enabled(&self) -> bool {
        let _sync_lengths = self.latest_sync_length.borrow();

        // TODO: Determine if the mempool is actually enabled or not (#2592).
        true
    }
}
