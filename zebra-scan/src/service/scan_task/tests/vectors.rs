//! Fixed test vectors for the scan task.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use color_eyre::Report;

use zebra_chain::{block::Height, transaction};
use zebra_node_services::scan_service::response::ScanResult;

use crate::{service::ScanTask, tests::mock_sapling_scanning_keys};

/// Test that [`ScanTask::process_messages`] adds and removes keys as expected for `RegisterKeys` and `DeleteKeys` command
#[tokio::test]
async fn scan_task_processes_messages_correctly() -> Result<(), Report> {
    let (mut mock_scan_task, mut cmd_receiver) = ScanTask::mock();
    let mut parsed_keys = HashMap::new();
    let network = &Default::default();

    // Send some keys to be registered
    let num_keys = 10;
    let sapling_keys =
        mock_sapling_scanning_keys(num_keys.try_into().expect("should fit in u8"), network);
    let sapling_keys_with_birth_heights: Vec<(String, Option<u32>)> =
        sapling_keys.into_iter().zip((0..).map(Some)).collect();
    mock_scan_task.register_keys(sapling_keys_with_birth_heights.clone())?;

    let (new_keys, _new_results_senders, _new_results_receivers) =
        ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, network)?;

    // Check that it updated parsed_keys correctly and returned the right new keys when starting with an empty state

    assert_eq!(
        new_keys.len(),
        num_keys,
        "should add all received keys to new keys"
    );

    assert_eq!(
        parsed_keys.len(),
        num_keys,
        "should add all received keys to parsed keys"
    );

    mock_scan_task.register_keys(sapling_keys_with_birth_heights.clone())?;

    // Check that no key should be added if they are all already known and the heights are the same

    let (new_keys, _new_results_senders, _new_results_receivers) =
        ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, network)?;

    assert_eq!(
        parsed_keys.len(),
        num_keys,
        "should not add existing keys to parsed keys"
    );

    assert!(
        new_keys.is_empty(),
        "should not return known keys as new keys"
    );

    // Check that keys can't be overridden.

    let sapling_keys = mock_sapling_scanning_keys(20, network);
    let sapling_keys_with_birth_heights: Vec<(String, Option<u32>)> = sapling_keys
        .clone()
        .into_iter()
        .map(|key| (key, Some(0)))
        .collect();

    mock_scan_task.register_keys(sapling_keys_with_birth_heights[10..20].to_vec())?;
    mock_scan_task.register_keys(sapling_keys_with_birth_heights[10..15].to_vec())?;

    let (new_keys, _new_results_senders, _new_results_receivers) =
        ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, network)?;

    assert_eq!(
        parsed_keys.len(),
        20,
        "should not add existing keys to parsed keys"
    );

    assert_eq!(
        new_keys.len(),
        10,
        "should add 10 of received keys to new keys"
    );

    // Check that it removes keys correctly

    let sapling_keys = mock_sapling_scanning_keys(30, network);
    let done_rx = mock_scan_task.remove_keys(sapling_keys.clone())?;

    let (new_keys, _new_results_senders, _new_results_receivers) =
        ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, network)?;

    // Check that it sends the done notification successfully before returning and dropping `done_tx`
    done_rx.await?;

    assert!(
        parsed_keys.is_empty(),
        "all parsed keys should have been removed"
    );

    assert!(new_keys.is_empty(), "there should be no new keys");

    // Check that it doesn't return removed keys as new keys when processing a batch of messages

    mock_scan_task.register_keys(sapling_keys_with_birth_heights.clone())?;

    mock_scan_task.remove_keys(sapling_keys.clone())?;

    let (new_keys, _new_results_senders, _new_results_receivers) =
        ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, network)?;

    assert!(
        new_keys.is_empty(),
        "all registered keys should be removed before process_messages returns"
    );

    // Check that it does return registered keys if they were removed in a prior message when processing a batch of messages

    mock_scan_task.register_keys(sapling_keys_with_birth_heights.clone())?;

    mock_scan_task.remove_keys(sapling_keys.clone())?;

    mock_scan_task.register_keys(sapling_keys_with_birth_heights[..2].to_vec())?;

    let (new_keys, _new_results_senders, _new_results_receivers) =
        ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, network)?;

    assert_eq!(
        new_keys.len(),
        2,
        "should return 2 keys as new_keys after removals"
    );

    assert_eq!(
        parsed_keys.len(),
        2,
        "should add 2 keys to parsed_keys after removals"
    );

    let subscribe_keys: HashSet<String> = sapling_keys[..5].iter().cloned().collect();
    let result_receiver_fut = {
        let mut mock_scan_task = mock_scan_task.clone();
        tokio::spawn(async move {
            mock_scan_task
                .subscribe(subscribe_keys.clone())
                .expect("should send subscribe msg successfully")
                .await
        })
    };

    // Wait for spawned task to send subscribe message
    tokio::time::sleep(Duration::from_secs(1)).await;

    let (_new_keys, new_results_senders, new_results_receivers) =
        ScanTask::process_messages(&mut cmd_receiver, &mut parsed_keys, network)?;

    let (result_receiver, rsp_tx) = new_results_receivers
        .into_iter()
        .next()
        .expect("there should be a new result receiver");

    rsp_tx
        .send(result_receiver)
        .expect("should send response successfully");

    let mut result_receiver = result_receiver_fut
        .await
        .expect("tokio task should join successfully")
        .expect("should send and receive message successfully");

    let processed_subscribe_keys: HashSet<String> = new_results_senders.keys().cloned().collect();
    let expected_new_subscribe_keys: HashSet<String> = sapling_keys[..2].iter().cloned().collect();

    assert_eq!(
        processed_subscribe_keys, expected_new_subscribe_keys,
        "should return new result senders for registered keys"
    );

    for sender in new_results_senders.values() {
        // send a fake tx id for each key
        sender
            .send(ScanResult {
                key: String::new(),
                height: Height::MIN,
                tx_id: transaction::Hash([0; 32]),
            })
            .await?;
    }

    let mut num_results = 0;

    while result_receiver.try_recv().is_ok() {
        num_results += 1;
    }

    assert_eq!(
        num_results,
        expected_new_subscribe_keys.len(),
        "there should be a fake result sent for each subscribed key"
    );

    Ok(())
}
