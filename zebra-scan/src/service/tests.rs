//! Tests for ScanService.

use std::time::Duration;

use futures::{stream::FuturesOrdered, StreamExt};
use tokio::sync::mpsc::error::TryRecvError;
use tower::{timeout::error::Elapsed, Service, ServiceBuilder, ServiceExt};

use color_eyre::{eyre::eyre, Result};

use zebra_chain::{block::Height, parameters::Network};
use zebra_node_services::scan_service::{request::Request, response::Response};
use zebra_state::TransactionIndex;

use crate::{
    init::SCAN_SERVICE_TIMEOUT,
    service::{scan_task::ScanTaskCommand, ScanService},
    storage::db::tests::{fake_sapling_results, new_test_storage},
    tests::{mock_sapling_scanning_keys, ZECPAGES_SAPLING_VIEWING_KEY},
    Config,
};

/// Tests that keys are deleted correctly
#[tokio::test]
pub async fn scan_service_deletes_keys_correctly() -> Result<()> {
    let mut db = new_test_storage(&Network::Mainnet);

    let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();

    for fake_result_height in [Height::MIN, Height(1), Height::MAX] {
        db.insert_sapling_results(
            &zec_pages_sapling_efvk,
            fake_result_height,
            fake_sapling_results([
                TransactionIndex::MIN,
                TransactionIndex::from_index(40),
                TransactionIndex::MAX,
            ]),
        );
    }

    assert!(
        !db.sapling_results(&zec_pages_sapling_efvk).is_empty(),
        "there should be some results for this key in the db"
    );

    let (mut scan_service, mut cmd_receiver) = ScanService::new_with_mock_scanner(db);

    let response_fut = scan_service
        .ready()
        .await
        .map_err(|err| eyre!(err))?
        .call(Request::DeleteKeys(vec![zec_pages_sapling_efvk.clone()]));

    let expected_keys = vec![zec_pages_sapling_efvk.clone()];
    let cmd_handler_fut = tokio::spawn(async move {
        let Some(ScanTaskCommand::RemoveKeys { done_tx, keys }) = cmd_receiver.recv().await else {
            panic!("should successfully receive RemoveKeys message");
        };

        assert_eq!(keys, expected_keys, "keys should match the request keys");

        done_tx.send(()).expect("send should succeed");
    });

    // Poll futures
    let (response, join_result) = tokio::join!(response_fut, cmd_handler_fut);
    join_result?;

    match response.map_err(|err| eyre!(err))? {
        Response::DeletedKeys => {}
        _ => panic!("scan service returned unexpected response variant"),
    };

    assert!(
        scan_service
            .db
            .sapling_results(&zec_pages_sapling_efvk)
            .is_empty(),
        "all results for this key should have been deleted"
    );

    Ok(())
}

/// Tests that keys are deleted correctly
#[tokio::test]
pub async fn scan_service_subscribes_to_results_correctly() -> Result<()> {
    let db = new_test_storage(&Network::Mainnet);

    let (mut scan_service, mut cmd_receiver) = ScanService::new_with_mock_scanner(db);

    let keys = [String::from("fake key")];

    let response_fut = scan_service
        .ready()
        .await
        .map_err(|err| eyre!(err))?
        .call(Request::SubscribeResults(keys.iter().cloned().collect()));

    let expected_keys = keys.iter().cloned().collect();
    let cmd_handler_fut = tokio::spawn(async move {
        let Some(ScanTaskCommand::SubscribeResults { rsp_tx, keys }) = cmd_receiver.recv().await
        else {
            panic!("should successfully receive SubscribeResults message");
        };

        let (_results_sender, results_receiver) = tokio::sync::mpsc::channel(1);
        rsp_tx
            .send(results_receiver)
            .expect("should send response successfully");
        assert_eq!(keys, expected_keys, "keys should match the request keys");
    });

    // Poll futures
    let (response, join_result) = tokio::join!(response_fut, cmd_handler_fut);
    join_result?;

    let mut results_receiver = match response.map_err(|err| eyre!(err))? {
        Response::SubscribeResults(results_receiver) => results_receiver,
        _ => panic!("scan service returned unexpected response variant"),
    };

    assert_eq!(
        results_receiver.try_recv(),
        Err(TryRecvError::Disconnected),
        "channel with no items and dropped sender should be closed"
    );

    Ok(())
}

/// Tests that results are cleared are deleted correctly
#[tokio::test]
pub async fn scan_service_clears_results_correctly() -> Result<()> {
    let mut db = new_test_storage(&Network::Mainnet);

    let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();

    for fake_result_height in [Height::MIN, Height(1), Height::MAX] {
        db.insert_sapling_results(
            &zec_pages_sapling_efvk,
            fake_result_height,
            fake_sapling_results([
                TransactionIndex::MIN,
                TransactionIndex::from_index(40),
                TransactionIndex::MAX,
            ]),
        );
    }

    assert!(
        !db.sapling_results(&zec_pages_sapling_efvk).is_empty(),
        "there should be some results for this key in the db"
    );

    let (mut scan_service, _cmd_receiver) = ScanService::new_with_mock_scanner(db.clone());

    let response = scan_service
        .ready()
        .await
        .map_err(|err| eyre!(err))?
        .call(Request::ClearResults(vec![zec_pages_sapling_efvk.clone()]))
        .await
        .map_err(|err| eyre!(err))?;

    match response {
        Response::ClearedResults => {}
        _ => panic!("scan service returned unexpected response variant"),
    };

    assert_eq!(
        db.sapling_results(&zec_pages_sapling_efvk).len(),
        1,
        "all results for this key should have been deleted, one empty entry should remain"
    );

    for (_, result) in db.sapling_results(&zec_pages_sapling_efvk) {
        assert!(
            result.is_empty(),
            "there should be no results for this entry in the db"
        );
    }

    Ok(())
}

/// Tests that results for key are returned correctly
#[tokio::test]
pub async fn scan_service_get_results_for_key_correctly() -> Result<()> {
    let mut db = new_test_storage(&Network::Mainnet);

    let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();

    for fake_result_height in [Height::MIN, Height(1), Height::MAX] {
        db.insert_sapling_results(
            &zec_pages_sapling_efvk,
            fake_result_height,
            fake_sapling_results([
                TransactionIndex::MIN,
                TransactionIndex::from_index(40),
                TransactionIndex::MAX,
            ]),
        );
    }

    assert!(
        db.sapling_results(&zec_pages_sapling_efvk).len() == 3,
        "there should be 3 heights for this key in the db"
    );

    for (_height, transactions) in db.sapling_results(&zec_pages_sapling_efvk) {
        assert!(
            transactions.len() == 3,
            "there should be 3 transactions for each height for this key in the db"
        );
    }

    // We don't need to send any command to the scanner for this call.
    let (mut scan_service, _cmd_receiver) = ScanService::new_with_mock_scanner(db);

    let response_fut = scan_service
        .ready()
        .await
        .map_err(|err| eyre!(err))?
        .call(Request::Results(vec![zec_pages_sapling_efvk.clone()]));

    match response_fut.await.map_err(|err| eyre!(err))? {
        Response::Results(results) => {
            assert!(
                results.contains_key(&zec_pages_sapling_efvk),
                "results should contain the requested key"
            );
            assert!(results.len() == 1, "values are only for 1 key");

            assert!(
                results
                    .get_key_value(&zec_pages_sapling_efvk)
                    .unwrap()
                    .1
                    .len()
                    == 3,
                "we should have 3 heights for the given key "
            );

            for transactions in results
                .get_key_value(&zec_pages_sapling_efvk)
                .unwrap()
                .1
                .values()
            {
                assert!(
                    transactions.len() == 3,
                    "there should be 3 transactions for each height for this key"
                );
            }
        }
        _ => panic!("scan service returned unexpected response variant"),
    };

    Ok(())
}

/// Tests that the scan service registers keys correctly.
#[tokio::test]
pub async fn scan_service_registers_keys_correctly() -> Result<()> {
    for network in Network::iter() {
        scan_service_registers_keys_correctly_for(&network).await?;
    }

    Ok(())
}

async fn scan_service_registers_keys_correctly_for(network: &Network) -> Result<()> {
    // Mock the state.
    let (state, _, _, chain_tip_change) = zebra_state::populated_state(vec![], network).await;

    // Instantiate the scan service.
    let mut scan_service = ServiceBuilder::new()
        .buffer(2)
        .service(ScanService::new(&Config::ephemeral(), network, state, chain_tip_change).await);

    // Mock three Sapling keys.
    let mocked_keys = mock_sapling_scanning_keys(3, network);

    // Add birth heights to the mocked keys.
    let keys_to_register: Vec<_> = mocked_keys
        .clone()
        .into_iter()
        .zip((0u32..).map(Some))
        .collect();

    // Register the first key.
    match scan_service
        .ready()
        .await
        .map_err(|err| eyre!(err))?
        .call(Request::RegisterKeys(keys_to_register[..1].to_vec()))
        .await
        .map_err(|err| eyre!(err))?
    {
        Response::RegisteredKeys(registered_keys) => {
            // The key should be registered.
            assert_eq!(
                registered_keys,
                mocked_keys[..1],
                "response should match newly registered key"
            );
        }

        _ => panic!("scan service should have responded with the `RegisteredKeys` response"),
    }

    // Try registering all three keys.
    match scan_service
        .ready()
        .await
        .map_err(|err| eyre!(err))?
        .call(Request::RegisterKeys(keys_to_register))
        .await
        .map_err(|err| eyre!(err))?
    {
        Response::RegisteredKeys(registered_keys) => {
            // Only the last two keys should be registered in this service call since the first one
            // was registered in the previous call.
            assert_eq!(
                registered_keys,
                mocked_keys[1..3],
                "response should match newly registered keys"
            );
        }

        _ => panic!("scan service should have responded with the `RegisteredKeys` response"),
    }

    // Try registering invalid keys.
    let register_keys_error_message = scan_service
        .ready()
        .await
        .map_err(|err| eyre!(err))?
        .call(Request::RegisterKeys(vec![(
            "invalid key".to_string(),
            None,
        )]))
        .await
        .expect_err("response should be an error when there are no valid keys to be added")
        .to_string();

    assert!(
        register_keys_error_message.starts_with("no keys were registered"),
        "error message should say that no keys were registered"
    );

    Ok(())
}

/// Test that the scan service with a timeout layer returns timeout errors after expected timeout
#[tokio::test]
async fn scan_service_timeout() -> Result<()> {
    let db = new_test_storage(&Network::Mainnet);

    let (scan_service, _cmd_receiver) = ScanService::new_with_mock_scanner(db);
    let mut scan_service = ServiceBuilder::new()
        .buffer(10)
        .timeout(SCAN_SERVICE_TIMEOUT)
        .service(scan_service);

    let keys = vec![String::from("fake key")];
    let mut response_futs = FuturesOrdered::new();

    for request in [
        Request::RegisterKeys(keys.iter().cloned().map(|key| (key, None)).collect()),
        Request::SubscribeResults(keys.iter().cloned().collect()),
        Request::DeleteKeys(keys),
    ] {
        let response_fut = scan_service
            .ready()
            .await
            .expect("service should be ready")
            .call(request);

        response_futs.push_back(tokio::time::timeout(
            SCAN_SERVICE_TIMEOUT
                .checked_add(Duration::from_secs(1))
                .expect("should not overflow"),
            response_fut,
        ));
    }

    let expect_timeout_err = |response: Option<Result<Result<_, _>, _>>| {
        response
            .expect("response_futs should not be empty")
            .expect("service should respond with timeout error before outer timeout")
            .expect_err("service response should be a timeout error")
    };

    // RegisterKeys and SubscribeResults should return `Elapsed` errors from `Timeout` layer
    for _ in 0..2 {
        let response = response_futs.next().await;
        expect_timeout_err(response)
            .downcast::<Elapsed>()
            .expect("service should return Elapsed error from Timeout layer");
    }

    let response = response_futs.next().await;
    let response_error_msg = expect_timeout_err(response).to_string();

    assert!(
        response_error_msg.starts_with("request timed out"),
        "error message should say the request timed out"
    );

    Ok(())
}
