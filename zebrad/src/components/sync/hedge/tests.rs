//! Arm-lifetime controls for the sync hedge.

use std::{future, io, sync::atomic::AtomicBool, time::Duration};

use futures::FutureExt;
use tokio::time::timeout;
use tower::{hedge::Policy, limit::ConcurrencyLimit, retry::Retry, service_fn, ServiceExt};
use zebra_chain::block;
use zebra_test::mock_service::MockService;

use super::super::{
    downloads::BlockDownloadVerifyError, tests::vectors::new_chain_sync_with_misbehavior,
};
use super::*;

const BOUND: Duration = Duration::from_secs(1);

fn original() -> HedgeRequest {
    HedgeRequest {
        request: zn::Request::Peers,
        arm: Arm {
            state: Arc::new(ArmState {
                flags: AtomicU8::new(1),
                wakers: [AtomicWaker::new(), AtomicWaker::new()],
                error: Mutex::new(None),
            }),
            index: 0,
        },
    }
}

fn response(
    request: HedgeRequest,
    result: Result<(), BoxError>,
) -> ArmFuture<future::Ready<Result<(), BoxError>>> {
    ArmFuture {
        inner: Some(future::ready(result)),
        arm: request.arm,
        error: None,
    }
}

#[tokio::test(start_paused = true)]
async fn cold_histogram_and_retry_clones_do_not_keep_an_error_waiting() {
    let inner = service_fn(|_: zn::Request| future::ready(Err::<(), BoxError>("exhausted".into())));
    let network = SuccessHedge::new(Retry::new(zn::RetryLimit::new(3), inner));
    let result = timeout(BOUND, network.oneshot(zn::Request::Peers))
        .await
        .expect("a rejected cold hedge clone must unregister, even when Retry clones its payload");
    assert_eq!(result.unwrap_err().to_string(), "exhausted");
}

#[tokio::test(start_paused = true)]
async fn both_errors_finish_and_exhausted_arms_release_inner_capacity() {
    let original = original();
    let clone = HedgePolicy.clone_request(&original).unwrap();
    let inner = service_fn(|_: zn::Request| future::ready(Err::<(), BoxError>("exhausted".into())));
    let mut service = ArmService {
        inner: ConcurrencyLimit::new(inner, 1),
        ready_error: None,
    };
    let mut first = Box::pin(service.ready().await.unwrap().call(original));
    assert!(first.as_mut().now_or_never().is_none());
    let second = service
        .ready()
        .now_or_never()
        .expect("the exhausted arm must release its concurrency permit before waiting")
        .unwrap()
        .call(clone);
    assert!(second.now_or_never().unwrap().is_err());
    // Tower returns the aggregate error once, then drops its other arm.
    drop(first);
}

#[tokio::test(start_paused = true)]
async fn mixed_hedge_failures_requeue_missing_blocks_without_restart() {
    for primary_first in [true, false] {
        for not_found_first in [true, false] {
            for sibling_succeeds in [false, true] {
                let hash = block::Hash::from([0xDE; 32]);
                let request = zn::Request::BlockDownload {
                    hash,
                    advertiser: None,
                };
                let mut peers: MockService<zn::Request, (), _> =
                    MockService::build().for_unit_tests();
                let mut network = SuccessHedge(hedge::Hedge::new_with_mock_latencies(
                    ArmService {
                        inner: peers.clone(),
                        ready_error: None,
                    },
                    HedgePolicy,
                    20,
                    0.95,
                    2 * SYNC_RESTART_DELAY,
                    &[100; 20],
                ));
                let mut pending = Box::pin(network.ready().await.unwrap().call(request.clone()));
                assert!(pending.as_mut().now_or_never().is_none());
                let primary = peers.expect_request(request.clone()).await;
                tokio::time::advance(Duration::from_millis(100)).await;
                assert!(pending.as_mut().now_or_never().is_none());
                let delayed = peers.expect_request(request).await;
                let (first, second) = if primary_first {
                    (primary, delayed)
                } else {
                    (delayed, primary)
                };

                let missing = Box::new(io::Error::new(io::ErrorKind::NotFound, "NotFoundResponse"));
                let missing_ptr = std::ptr::from_ref(missing.as_ref());
                let missing: BoxError = missing;
                let reset: BoxError =
                    io::Error::new(io::ErrorKind::ConnectionReset, "connection reset").into();
                let (first_error, second_error) = if not_found_first {
                    (missing, reset)
                } else {
                    (reset, missing)
                };
                first.respond(Err::<(), _>(first_error));
                assert!(
                    pending.as_mut().now_or_never().is_none(),
                    "the first failure must leave its sibling alive",
                );
                second.respond(if sibling_succeeds {
                    Ok(())
                } else {
                    Err(second_error)
                });
                let result = timeout(BOUND, pending).await.unwrap();
                if sibling_succeeds {
                    assert!(
                        result.is_ok(),
                        "a successful sibling must win over any error"
                    );
                    continue;
                }

                let error = result.unwrap_err();
                assert!(
                    std::ptr::eq(error.downcast_ref::<io::Error>().unwrap(), missing_ptr),
                    "both completion orders must preserve the original boxed NotFound error",
                );
                let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
                chain_sync
                    .handle_block_response(Err(BlockDownloadVerifyError::DownloadFailed {
                        error,
                        hash,
                    }))
                    .expect("mixed failures with a missing block must not restart sync");
                assert!(
                    chain_sync.reobtain_hashes.contains(&hash),
                    "the aggregate failure must re-queue the missing block",
                );
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn dropping_an_unissued_clone_wakes_the_failed_arm() {
    let original = original();
    let clone = HedgePolicy.clone_request(&original).unwrap();
    let state = Arc::downgrade(&original.arm.state);
    let failed = tokio::spawn(response(original, Err("exhausted".into())));
    tokio::task::yield_now().await;
    assert!(!failed.is_finished());
    drop(clone);
    assert!(timeout(BOUND, failed).await.unwrap().unwrap().is_err());
    assert!(
        state.upgrade().is_none(),
        "completed arms must release all ownership"
    );
}

#[tokio::test(start_paused = true)]
async fn canceling_tower_drops_the_active_and_delayed_arms() {
    let request = original();
    let state = Arc::downgrade(&request.arm.state);
    let mut network = hedge::Hedge::new_with_mock_latencies(
        ArmService {
            inner: service_fn(|_: zn::Request| future::pending::<Result<(), BoxError>>()),
            ready_error: None,
        },
        HedgePolicy,
        20,
        0.95,
        2 * SYNC_RESTART_DELAY,
        &[100; 20],
    );
    let mut pending = Box::pin(network.ready().await.unwrap().call(request));
    assert!(pending.as_mut().now_or_never().is_none());
    drop(pending);
    assert!(
        state.upgrade().is_none(),
        "cancellation must release both arm owners"
    );
}

#[derive(Clone)]
struct ReadinessError<S> {
    inner: S,
    failed: Arc<AtomicBool>,
}

impl<S: Service<zn::Request, Error = BoxError>> Service<zn::Request> for ReadinessError<S> {
    type Response = S::Response;
    type Error = BoxError;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.failed.load(Ordering::Acquire) {
            Poll::Ready(Err("readiness failed".into()))
        } else {
            self.inner.poll_ready(cx)
        }
    }

    fn call(&mut self, request: zn::Request) -> Self::Future {
        assert!(
            !self.failed.load(Ordering::Acquire),
            "a failed readiness check must not dispatch a request",
        );
        self.inner.call(request)
    }
}

#[tokio::test(start_paused = true)]
async fn cold_readiness_errors_finish_without_dispatching() {
    let network = SuccessHedge::new(ReadinessError {
        inner: service_fn(|_: zn::Request| future::ready(Ok::<(), BoxError>(()))),
        failed: Arc::new(AtomicBool::new(true)),
    });
    let result = timeout(BOUND, network.oneshot(zn::Request::Peers))
        .await
        .expect("a cold readiness failure must not wait for an unissued clone");
    assert_eq!(result.unwrap_err().to_string(), "readiness failed");
}

#[tokio::test(start_paused = true)]
async fn delayed_readiness_errors_cannot_cancel_a_live_success() {
    let failed = Arc::new(AtomicBool::new(false));
    let mut network = hedge::Hedge::new_with_mock_latencies(
        ArmService {
            inner: ReadinessError {
                inner: service_fn(|_: zn::Request| async {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Ok::<(), BoxError>(())
                }),
                failed: Arc::clone(&failed),
            },
            ready_error: None,
        },
        HedgePolicy,
        20,
        0.95,
        2 * SYNC_RESTART_DELAY,
        &[100; 20],
    );
    let mut pending = Box::pin(network.ready().await.unwrap().call(original()));
    assert!(pending.as_mut().now_or_never().is_none());
    failed.store(true, Ordering::Release);
    tokio::time::advance(Duration::from_millis(100)).await;
    assert!(
        pending.as_mut().now_or_never().is_none(),
        "the delayed readiness error must leave the first response alive",
    );
    assert!(timeout(BOUND, pending).await.unwrap().is_ok());
}

#[tokio::test(start_paused = true)]
async fn readiness_errors_release_single_concurrency_reservation() {
    let network = hedge::Hedge::new_with_mock_latencies(
        ArmService {
            inner: ConcurrencyLimit::new(
                ReadinessError {
                    inner: service_fn(|_: zn::Request| future::ready(Ok::<(), BoxError>(()))),
                    failed: Arc::new(AtomicBool::new(true)),
                },
                1,
            ),
            ready_error: None,
        },
        HedgePolicy,
        20,
        0.95,
        2 * SYNC_RESTART_DELAY,
        &[100; 20],
    );
    let result = timeout(BOUND, network.oneshot(original()))
        .await
        .expect("a failed primary readiness reservation must not strand the delayed hedge");
    assert_eq!(result.unwrap_err().to_string(), "readiness failed");
}
