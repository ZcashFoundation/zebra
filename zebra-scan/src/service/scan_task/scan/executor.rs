//! The scan task executor

use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tracing::Instrument;
use zebra_chain::BoxError;

use super::add_keys::ScanRangeTaskBuilder;

pub fn spawn_init() -> (
    UnboundedSender<ScanRangeTaskBuilder>,
    JoinHandle<Result<(), BoxError>>,
) {
    let (scan_task_sender, scan_task_receiver) = tokio::sync::mpsc::unbounded_channel();
    (
        scan_task_sender,
        tokio::spawn(scan_task_executor(scan_task_receiver).in_current_span()),
    )
}

pub async fn scan_task_executor(
    mut scan_task_receiver: UnboundedReceiver<ScanRangeTaskBuilder>,
) -> Result<(), BoxError> {
    let mut scan_range_tasks = FuturesUnordered::new();

    // Push a pending future so that `.next()` will always return `Some`
    scan_range_tasks.push(tokio::spawn(
        std::future::pending::<Result<(), BoxError>>().boxed(),
    ));

    loop {
        tokio::select! {
            Some(scan_range_task) = scan_task_receiver.recv() => {
                // TODO: Add a long timeout?
                scan_range_tasks.push(scan_range_task.spawn());
            }

            Some(finished_task) = scan_range_tasks.next() => {
                // Return early if there's an error
                finished_task.expect("futures unordered with pending future should always return Some")?;
            }
        }
    }
}
