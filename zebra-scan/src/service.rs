//! [`tower::Service`] for zebra-scan.

use std::{future::Future, pin::Pin, task::Poll, time::Duration};

use futures::future::FutureExt;
use tower::Service;

use zebra_chain::parameters::Network;
use zebra_state::ChainTipChange;

use crate::{init::ScanTask, scan, storage::Storage, Config, Request, Response};

#[cfg(test)]
mod tests;

/// Zebra-scan [`tower::Service`]
#[derive(Debug)]
pub struct ScanService {
    /// On-disk storage
    pub db: Storage,

    /// Handle to scan task that's responsible for writing results
    scan_task: ScanTask,
}

/// A timeout applied to `DeleteKeys` requests.
const DELETE_KEY_TIMEOUT: Duration = Duration::from_secs(15);

impl ScanService {
    /// Create a new [`ScanService`].
    pub fn new(
        config: &Config,
        network: Network,
        state: scan::State,
        chain_tip_change: ChainTipChange,
    ) -> Self {
        Self {
            db: Storage::new(config, network, false),
            scan_task: ScanTask::spawn(config, network, state, chain_tip_change),
        }
    }

    /// Create a new [`ScanService`] with a mock `ScanTask`
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn new_with_mock_scanner(
        db: Storage,
    ) -> (
        Self,
        std::sync::mpsc::Receiver<crate::init::ScanTaskCommand>,
    ) {
        let (scan_task, cmd_receiver) = ScanTask::mock();
        (Self { db, scan_task }, cmd_receiver)
    }
}

impl Service<Request> for ScanService {
    type Response = Response;
    type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        // TODO: If scan task returns an error, add error to the panic message
        assert!(
            !self.scan_task.handle.is_finished(),
            "scan task finished unexpectedly"
        );

        self.db.check_for_panics();

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        if let Err(error) = req.check() {
            return async move { Err(error) }.boxed();
        }

        match req {
            Request::Info => {
                let db = self.db.clone();

                return async move {
                    Ok(Response::Info {
                        min_sapling_birthday_height: db.min_sapling_birthday_height(),
                    })
                }
                .boxed();
            }

            Request::CheckKeyHashes(_key_hashes) => {
                // TODO: check that these entries exist in db
            }

            Request::RegisterKeys(_viewing_key_with_hashes) => {
                // TODO:
                //  - add these keys as entries in db
                //  - send new keys to scan task
            }

            Request::DeleteKeys(keys) => {
                let mut db = self.db.clone();
                let mut scan_task = self.scan_task.clone();

                return async move {
                    // Wait for a message to confirm that the scan task has removed the key up to `DELETE_KEY_TIMEOUT`
                    let remove_keys_result =
                        tokio::time::timeout(DELETE_KEY_TIMEOUT, scan_task.remove_keys(&keys)?)
                            .await
                            .map_err(|_| "timeout waiting for delete keys done notification");

                    // Delete the key from the database after either confirmation that it's been removed from the scan task, or
                    // waiting `DELETE_KEY_TIMEOUT`.
                    let delete_key_task = tokio::task::spawn_blocking(move || {
                        db.delete_sapling_keys(keys);
                    });

                    // Return timeout errors or `RecvError`s, or wait for the key to be deleted from the database.
                    remove_keys_result??;
                    delete_key_task.await?;

                    Ok(Response::DeletedKeys)
                }
                .boxed();
            }

            Request::Results(_key_hashes) => {
                // TODO: read results from db
            }

            Request::SubscribeResults(_key_hashes) => {
                // TODO: send key_hashes and mpsc::Sender to scanner task, return mpsc::Receiver to caller
            }

            Request::ClearResults(keys) => {
                let mut db = self.db.clone();

                return async move {
                    // Delete the key from the database after either confirmation
                    tokio::task::spawn_blocking(move || {
                        db.delete_sapling_results(keys);
                    })
                    .await?;

                    Ok(Response::ClearedResults)
                }
                .boxed();
            }
        }

        async move { Ok(Response::Results(vec![])) }.boxed()
    }
}
