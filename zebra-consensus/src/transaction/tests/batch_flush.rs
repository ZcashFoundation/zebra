//! Async check polling and result-completeness regressions at block flush boundaries.

use std::{
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::future::poll_fn;
use tokio::sync::oneshot;

use crate::{primitives, transaction::AsyncChecks, BoxError};

#[tokio::test]
async fn boundary_polls_checks_before_waiting_and_still_requires_every_result() {
    let context = Arc::new(());
    let key = primitives::block_verifier_batch_flush_key(&context);
    let _guard = primitives::register_block_verifier_batch_flush(&context, 1);
    let started = Arc::new(AtomicUsize::new(0));
    let mut checks = AsyncChecks::new();
    let (first_tx, first_rx) = oneshot::channel::<()>();
    let (second_tx, second_rx) = oneshot::channel::<()>();
    for receiver in [first_rx, second_rx] {
        let started = started.clone();
        checks.push(async move {
            started.fetch_add(1, Ordering::SeqCst);
            receiver.await.map_err(BoxError::from)
        });
    }
    let verification = checks.check(Some(key));
    tokio::pin!(verification);
    poll_fn(|cx| {
        assert!(verification.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(started.load(Ordering::SeqCst), 2);
    first_tx.send(()).unwrap();
    poll_fn(|cx| {
        assert!(verification.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    second_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), verification)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn boundary_does_not_hide_a_failed_check() {
    let context = Arc::new(());
    let key = primitives::block_verifier_batch_flush_key(&context);
    let _guard = primitives::register_block_verifier_batch_flush(&context, 1);
    let mut checks = AsyncChecks::new();
    checks.push(futures::future::pending());
    checks.push(async { Err::<(), BoxError>("invalid spend".into()) });
    let error = tokio::time::timeout(Duration::from_secs(5), checks.check(Some(key)))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.to_string(), "invalid spend");
}
