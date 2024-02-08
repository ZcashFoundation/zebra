//! Fixed test vectors for the scan task.

use std::collections::HashMap;

use color_eyre::Report;

use crate::{service::ScanTask, tests::mock_sapling_viewing_keys};

/// Test that [`ScanTask::process_messages`] adds and removes keys as expected for `RegisterKeys` and `DeleteKeys` command
#[tokio::test]
async fn scan_task_processes_messages_correctly() -> Result<(), Report> {
    let (mut mock_scan_task, cmd_receiver) = ScanTask::mock();
    let mut parsed_keys = HashMap::new();
    let network = Default::default();

    // Send some keys to be registered
    let num_keys = 10;
    let sapling_keys = mock_sapling_viewing_keys(num_keys as u8);
    let sapling_keys_with_birth_heights: Vec<(String, Option<u32>)> =
        sapling_keys.into_iter().zip((0..).map(Some)).collect();
    mock_scan_task.register_keys(sapling_keys_with_birth_heights.clone())?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys, network)?;

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

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys, network)?;

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

    let sapling_keys = mock_sapling_viewing_keys(20);
    let sapling_keys_with_birth_heights: Vec<(String, Option<u32>)> = sapling_keys
        .clone()
        .into_iter()
        .map(|key| (key, Some(0)))
        .collect();

    mock_scan_task.register_keys(sapling_keys_with_birth_heights[10..20].to_vec())?;
    mock_scan_task.register_keys(sapling_keys_with_birth_heights[10..15].to_vec())?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys, network)?;

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

    let sapling_keys = mock_sapling_viewing_keys(30);

    let done_rx = mock_scan_task.remove_keys(&sapling_keys)?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys, network)?;

    // Check that it sends the done notification successfully before returning and dropping `done_tx`
    done_rx.await?;

    assert!(
        parsed_keys.is_empty(),
        "all parsed keys should have been removed"
    );

    assert!(new_keys.is_empty(), "there should be no new keys");

    // Check that it doesn't return removed keys as new keys when processing a batch of messages

    mock_scan_task.register_keys(sapling_keys_with_birth_heights.clone())?;

    mock_scan_task.remove_keys(&sapling_keys)?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys, network)?;

    assert!(
        new_keys.is_empty(),
        "all registered keys should be removed before process_messages returns"
    );

    // Check that it does return registered keys if they were removed in a prior message when processing a batch of messages

    mock_scan_task.register_keys(sapling_keys_with_birth_heights.clone())?;

    mock_scan_task.remove_keys(&sapling_keys)?;

    mock_scan_task.register_keys(sapling_keys_with_birth_heights[..2].to_vec())?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys, network)?;

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

    Ok(())
}
