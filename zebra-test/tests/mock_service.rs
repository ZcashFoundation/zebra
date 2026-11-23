//! Tests for the [`zebra_test::mock_service`] module.

use std::{task::Poll, time::Duration};

use futures::{pin_mut, poll};
use tokio::time::{advance, sleep, Instant};
use tower::Service;

use zebra_test::mock_service::{MockService, DEFAULT_REQUEST_OBSERVATION_TIMEOUT};

#[tokio::test]
#[should_panic(expected = "MockService dropped 1 requests: increase with_proxy_channel_size")]
async fn lagged_requests_panic() {
    let mut mock: MockService<_, (), _> = MockService::build()
        .with_proxy_channel_size(1)
        .for_unit_tests();

    drop(mock.call(1));
    drop(mock.call(2));

    let _request = mock.try_next_request().await;
}

#[tokio::test(start_paused = true)]
async fn claimed_requests_do_not_restart_deadline() {
    let mut mock: MockService<_, (), _> = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let mut consumer = mock.clone();
    let mut caller = mock.clone();
    let next_request = mock.try_next_request();
    pin_mut!(next_request);
    assert!(poll!(&mut next_request).is_pending());

    advance(Duration::from_secs(1)).await;
    let response = caller.call("claimed");
    consumer.expect_request("claimed").await.respond(());
    response.await.unwrap();
    assert!(poll!(&mut next_request).is_pending());

    advance(Duration::from_millis(1001)).await;
    assert!(matches!(poll!(&mut next_request), Poll::Ready(None)));
}

/// Both assertion modes allow expected requests to arrive after the observation timeout.
#[tokio::test(start_paused = true)]
async fn positive_expectations_allow_delayed_requests() {
    let _init_guard = zebra_test::init();

    let mut mock: MockService<_, (), _> = MockService::build().for_unit_tests();
    let mut caller = mock.clone();
    tokio::join!(
        async {
            for request in [1, 2] {
                sleep(Duration::from_secs(1)).await;
                caller.call(request).await.unwrap();
            }
        },
        async {
            mock.expect_request(1).await.respond(());
            mock.expect_request_that(|request| *request == 2)
                .await
                .respond(());
        },
    );

    let mut mock: MockService<_, (), _> = MockService::build().for_prop_tests();
    let mut caller = mock.clone();
    tokio::join!(
        async {
            for request in [1, 2] {
                sleep(Duration::from_secs(1)).await;
                caller.call(request).await.unwrap();
            }
        },
        async {
            mock.expect_request(1).await.unwrap().respond(());
            mock.expect_request_that(|request| *request == 2)
                .await
                .unwrap()
                .respond(());
        },
    );
}

/// Both assertion modes check for absence using the short observation timeout.
#[tokio::test(start_paused = true)]
async fn absence_expectations_use_observation_timeout() {
    let _init_guard = zebra_test::init();

    let mut mock: MockService<(), (), _> = MockService::build().for_unit_tests();
    let start = Instant::now();

    mock.expect_no_requests().await;

    assert!(
        (DEFAULT_REQUEST_OBSERVATION_TIMEOUT..Duration::from_secs(1)).contains(&start.elapsed())
    );

    let mut mock: MockService<(), (), _> = MockService::build().for_prop_tests();
    let start = Instant::now();

    mock.expect_no_requests().await.unwrap();

    assert!(
        (DEFAULT_REQUEST_OBSERVATION_TIMEOUT..Duration::from_secs(1)).contains(&start.elapsed())
    );
}

/// An optional read returns no request after the short observation timeout.
#[tokio::test(start_paused = true)]
async fn optional_reads_use_observation_timeout() {
    let _init_guard = zebra_test::init();

    let mut mock: MockService<(), (), _> = MockService::build().for_unit_tests();
    let start = Instant::now();

    assert!(mock.try_next_request().await.is_none());

    assert!(
        (DEFAULT_REQUEST_OBSERVATION_TIMEOUT..Duration::from_secs(1)).contains(&start.elapsed())
    );
}

#[tokio::test(start_paused = true)]
async fn custom_deadline_applies_to_positive_and_absence_checks() {
    let delay = Duration::from_secs(2);
    let mock: MockService<(), (), _> = MockService::build()
        .with_max_request_delay(delay)
        .for_prop_tests();
    let clone = mock.clone();

    for mut mock in [mock, clone] {
        let start = Instant::now();
        assert!(mock.expect_request(()).await.is_err());
        assert!((delay..2 * delay).contains(&start.elapsed()));

        let start = Instant::now();
        mock.expect_no_requests().await.unwrap();
        assert!((delay..2 * delay).contains(&start.elapsed()));

        let start = Instant::now();
        assert!(mock.try_next_request().await.is_none());
        assert!((delay..2 * delay).contains(&start.elapsed()));
    }
}
