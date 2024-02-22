//! The scan task executor

use std::{collections::HashMap, sync::Arc};

use color_eyre::eyre::Report;
use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use tokio::{
    sync::{
        mpsc::{Receiver, Sender},
        watch,
    },
    task::JoinHandle,
};
use tracing::Instrument;
use zebra_node_services::scan_service::response::ScanResult;

use super::scan::ScanRangeTaskBuilder;

const EXECUTOR_BUFFER_SIZE: usize = 100;

pub fn spawn_init(
    subscribed_keys_receiver: watch::Receiver<Arc<HashMap<String, Sender<ScanResult>>>>,
) -> (Sender<ScanRangeTaskBuilder>, JoinHandle<Result<(), Report>>) {
    let (scan_task_sender, scan_task_receiver) = tokio::sync::mpsc::channel(EXECUTOR_BUFFER_SIZE);

    (
        scan_task_sender,
        tokio::spawn(
            scan_task_executor(scan_task_receiver, subscribed_keys_receiver).in_current_span(),
        ),
    )
}

pub async fn scan_task_executor(
    mut scan_task_receiver: Receiver<ScanRangeTaskBuilder>,
    subscribed_keys_receiver: watch::Receiver<Arc<HashMap<String, Sender<ScanResult>>>>,
) -> Result<(), Report> {
    let mut scan_range_tasks = FuturesUnordered::new();

    // Push a pending future so that `.next()` will always return `Some`
    scan_range_tasks.push(tokio::spawn(
        std::future::pending::<Result<(), Report>>().boxed(),
    ));

    loop {
        tokio::select! {
            Some(scan_range_task) = scan_task_receiver.recv() => {
                // TODO: Add a long timeout?
                scan_range_tasks.push(scan_range_task.spawn(subscribed_keys_receiver.clone()));
            }

            Some(finished_task) = scan_range_tasks.next() => {
                // Return early if there's an error
                finished_task.expect("futures unordered with pending future should always return Some")?;
            }
        }
    }
}
