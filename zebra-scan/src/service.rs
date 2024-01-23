//! [`tower::Service`] for zebra-scan.

use std::{future::Future, pin::Pin, task::Poll};

use tower::Service;
use zebra_chain::parameters::Network;
use zebra_state::ChainTipChange;

use crate::{init::ScanTask, scan, storage::Storage, Config, Request, Response};

/// Zebra-scan [`tower::Service`]
#[derive(Debug)]
pub struct ScanService {
    /// On-disk storage
    db: Storage,

    /// Handle to scan task that's responsible for writing results
    scan_task: ScanTask,
}

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
}

impl Service<Request> for ScanService {
    type Response = Response;
    type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        // TODO: Check for panics in scan task

        self.db.check_for_panics();

        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::CheckKeyHashes(key_hashes) => {
                // TODO: check that these entries exist in db
                todo!()
            }

            Request::RegisterKeys(viewing_key_with_hashes) => {
                // TODO:
                //  - add these keys as entries in db
                //  - send keys to scanner task
                todo!()
            }

            Request::DeleteKeys(key_hashes) => {
                // TODO: delete these entries from db
                todo!()
            }

            Request::Results(key_hashes) => {
                // TODO: read results from db
                todo!()
            }

            Request::SubscribeResults(key_hashes) => {
                // TODO: send key_hashes and mpsc::Sender to scanner task, return mpsc::Receiver to caller
                todo!()
            }

            Request::ClearResults(key_hashes) => {
                // TODO: clear results from db
                todo!()
            }
        }
    }
}
