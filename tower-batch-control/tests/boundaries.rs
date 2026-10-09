//! Deterministic explicit-flush, weight, and worker-readiness regression tests.

use std::{future, time::Duration};

use futures::FutureExt;
use tokio_test::{assert_pending, assert_ready, task};
use tower::{Service, ServiceExt};
use tower_batch_control::{Batch, BatchControl, RequestWeight};
use tower_test::mock;

#[derive(Debug)]
struct Weighted(usize);

impl RequestWeight for Weighted {
    fn request_weight(&self) -> usize {
        self.0
    }
}

#[tokio::test]
async fn boundary_flush_follows_items_without_waiting_for_timer() {
    let (inner, mut handle) = mock::pair::<BatchControl<Weighted>, ()>();
    let (mut batch, worker) = Batch::pair(inner, 64, 1, Duration::from_secs(3600));
    let mut worker = task::spawn(worker.run());
    let mut first = task::spawn(batch.ready().await.unwrap().call(Weighted(1)));
    let mut second = task::spawn(batch.ready().await.unwrap().call(Weighted(1)));
    assert!(batch.try_flush().unwrap());

    assert_pending!(worker.poll());
    let (request, first_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Item(Weighted(1))));
    let (request, second_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Item(Weighted(1))));
    let (request, flush_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Flush));

    // A flush command does not substitute for either verification result.
    flush_reply.send_response(());
    assert_pending!(first.poll());
    assert_pending!(second.poll());
    first_reply.send_response(());
    assert_ready!(first.poll()).unwrap();
    assert_pending!(second.poll());
    second_reply.send_response(());
    assert_ready!(second.poll()).unwrap();
}

#[tokio::test]
async fn explicit_flush_preserves_item_failures() {
    let (inner, mut handle) = mock::pair::<BatchControl<Weighted>, ()>();
    let (mut batch, worker) = Batch::pair(inner, 64, 1, Duration::from_secs(3600));
    let mut worker = task::spawn(worker.run());
    let mut response = task::spawn(batch.ready().await.unwrap().call(Weighted(1)));
    assert!(batch.try_flush().unwrap());
    assert_pending!(worker.poll());
    let (_, reply) = handle.next_request().await.unwrap();
    let (request, flush_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Flush));
    flush_reply.send_response(());
    reply.send_error("invalid proof");
    assert_eq!(
        assert_ready!(response.poll()).unwrap_err().to_string(),
        "invalid proof"
    );
}

#[tokio::test(start_paused = true)]
async fn saturation_retains_timed_flushing() {
    let (inner, mut handle) = mock::pair::<BatchControl<Weighted>, ()>();
    let (mut batch, worker) = Batch::pair(inner, 2, 1, Duration::from_millis(100));
    let mut worker = task::spawn(worker.run());
    let mut response = task::spawn(batch.ready().await.unwrap().call(Weighted(1)));
    // Hold the other reservation, leaving no room for a best-effort flush.
    let mut reservation = batch.clone();
    reservation.ready().await.unwrap();
    assert!(!batch.try_flush().unwrap());
    assert_pending!(worker.poll());
    let (request, reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Item(_)));
    tokio::time::advance(Duration::from_millis(100)).await;
    assert_pending!(worker.poll());
    let (request, flush_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Flush));
    flush_reply.send_response(());
    reply.send_response(());
    assert_ready!(response.poll()).unwrap();
}

#[tokio::test]
async fn request_weights_saturate_and_zero_weight_still_flushes() {
    let (inner, mut handle) = mock::pair::<BatchControl<Weighted>, ()>();
    // The queue-capacity product must not overflow or exceed Semaphore::MAX_PERMITS.
    let (mut batch, worker) = Batch::pair(inner, usize::MAX, 64, Duration::from_secs(3600));
    let mut worker = task::spawn(worker.run());
    let first = batch.ready().await.unwrap().call(Weighted(1));
    let second = batch.ready().await.unwrap().call(Weighted(usize::MAX));
    assert_pending!(worker.poll());
    let (_, first_reply) = handle.next_request().await.unwrap();
    let (_, second_reply) = handle.next_request().await.unwrap();
    let (request, flush_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Flush));
    first_reply.send_response(());
    second_reply.send_response(());
    flush_reply.send_response(());
    first.await.unwrap();
    second.await.unwrap();
    assert_pending!(worker.poll());

    let zero = batch.ready().await.unwrap().call(Weighted(0));
    assert!(batch.try_flush().unwrap());
    assert_pending!(worker.poll());
    let (_, reply) = handle.next_request().await.unwrap();
    let (request, flush_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Flush));
    reply.send_response(());
    flush_reply.send_response(());
    zero.await.unwrap();
}

#[tokio::test]
async fn zero_concurrency_configuration_still_processes_requests() {
    let inner = tower::service_fn(|_: BatchControl<()>| future::ready(Ok::<_, &'static str>(())));
    let mut batch = Batch::new(inner, 1, 0, Duration::from_secs(3600));
    tokio::time::timeout(
        Duration::from_secs(5),
        batch.ready().await.unwrap().call(()),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn completed_worker_readiness_is_repeatable_across_clones() {
    let inner = tower::service_fn(|_: BatchControl<()>| future::ready(Ok::<_, &'static str>(())));
    let (mut batch, worker) = Batch::pair(inner, 1, 1, Duration::from_secs(3600));
    let handle = tokio::spawn(worker.run());
    handle.abort();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    batch.register_worker(handle);
    let mut clone = batch.clone();
    assert!(batch.ready().await.is_err());
    assert!(batch.ready().await.is_err());
    assert!(clone.ready().await.is_err());
}

#[tokio::test]
async fn worker_panic_unwinds_once_without_poisoning_clone_readiness() {
    let inner = tower::service_fn(|_: BatchControl<()>| future::ready(Ok::<_, &'static str>(())));
    let (mut batch, worker) = Batch::pair(inner, 1, 1, Duration::from_secs(3600));
    drop(worker);
    let handle = tokio::spawn(async {
        panic!("worker panic");
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !handle.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    batch.register_worker(handle);
    let mut clone = batch.clone();
    assert!(std::panic::AssertUnwindSafe(batch.ready())
        .catch_unwind()
        .await
        .is_err());
    assert!(batch.ready().await.is_err());
    assert!(clone.ready().await.is_err());
}

#[tokio::test]
async fn overlapping_block_boundaries_keep_every_queued_item() {
    let (inner, mut handle) = mock::pair::<BatchControl<Weighted>, ()>();
    let (mut first_block, worker) = Batch::pair(inner, 64, 2, Duration::from_secs(3600));
    let mut second_block = first_block.clone();
    let mut worker = task::spawn(worker.run());
    let first = first_block.ready().await.unwrap().call(Weighted(1));
    assert!(first_block.try_flush().unwrap());
    let second = second_block.ready().await.unwrap().call(Weighted(2));
    assert!(second_block.try_flush().unwrap());
    assert_pending!(worker.poll());
    let (request, first_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Item(Weighted(1))));
    let (request, first_flush) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Flush));
    let (request, second_reply) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Item(Weighted(2))));
    let (request, second_flush) = handle.next_request().await.unwrap();
    assert!(matches!(request, BatchControl::Flush));
    first_reply.send_response(());
    second_reply.send_response(());
    first_flush.send_response(());
    second_flush.send_response(());
    first.await.unwrap();
    second.await.unwrap();
}
