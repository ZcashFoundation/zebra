//! Fixed test vectors for the scan task.

use std::collections::HashMap;

use color_eyre::Report;

use zebra_chain::block::Height;

use crate::service::ScanTask;

/// Test that [`ScanTask::process_messages`] adds and removes keys as expected for `RegisterKeys` and `DeleteKeys` command
#[tokio::test]
async fn scan_task_processes_messages_correctly() -> Result<(), Report> {
    let (mut mock_scan_task, cmd_receiver) = ScanTask::mock();
    let mut parsed_keys = HashMap::new();

    // Send some keys to be registered
    let num_keys = 10;
    mock_scan_task.register_keys(
        (0..num_keys)
            .map(|i| (i.to_string(), (vec![], vec![], Height::MIN)))
            .collect(),
    )?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys)?;

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

    mock_scan_task.register_keys(
        (0..num_keys)
            .map(|i| (i.to_string(), (vec![], vec![], Height::MIN)))
            .collect(),
    )?;

    // Check that no key should be added if they are all already known and the heights are the same

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys)?;

    assert_eq!(
        parsed_keys.len(),
        num_keys,
        "should not add existing keys to parsed keys"
    );

    assert!(
        new_keys.is_empty(),
        "should not return known keys as new keys"
    );

    // Check that it returns the last seen start height for a key as the new key when receiving 2 register key messages

    mock_scan_task.register_keys(
        (10..20)
            .map(|i| (i.to_string(), (vec![], vec![], Height::MIN)))
            .collect(),
    )?;

    mock_scan_task.register_keys(
        (10..15)
            .map(|i| (i.to_string(), (vec![], vec![], Height::MAX)))
            .collect(),
    )?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys)?;

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

    for (new_key, (_, _, start_height)) in new_keys {
        if (10..15).contains(&new_key.parse::<i32>().expect("should parse into int")) {
            assert_eq!(
                start_height,
                Height::MAX,
                "these key heights should have been overwritten by the second message"
            );
        }
    }

    // Check that it removes keys correctly

    let done_rx =
        mock_scan_task.remove_keys(&(0..200).map(|i| i.to_string()).collect::<Vec<_>>())?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys)?;

    // Check that it sends the done notification successfully before returning and dropping `done_tx`
    done_rx.await?;

    assert!(
        parsed_keys.is_empty(),
        "all parsed keys should have been removed"
    );

    assert!(new_keys.is_empty(), "there should be no new keys");

    // Check that it doesn't return removed keys as new keys when processing a batch of messages

    mock_scan_task.register_keys(
        (0..200)
            .map(|i| (i.to_string(), (vec![], vec![], Height::MAX)))
            .collect(),
    )?;

    mock_scan_task.remove_keys(&(0..200).map(|i| i.to_string()).collect::<Vec<_>>())?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys)?;

    assert!(
        new_keys.is_empty(),
        "all registered keys should be removed before process_messages returns"
    );

    // Check that it does return registered keys if they were removed in a prior message when processing a batch of messages

    mock_scan_task.register_keys(
        (0..200)
            .map(|i| (i.to_string(), (vec![], vec![], Height::MAX)))
            .collect(),
    )?;

    mock_scan_task.remove_keys(&(0..200).map(|i| i.to_string()).collect::<Vec<_>>())?;

    mock_scan_task.register_keys(
        (0..2)
            .map(|i| (i.to_string(), (vec![], vec![], Height::MAX)))
            .collect(),
    )?;

    let new_keys = ScanTask::process_messages(&cmd_receiver, &mut parsed_keys)?;

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
