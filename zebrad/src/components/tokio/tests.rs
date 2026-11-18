//! Shutdown lifecycle tests.

use super::*;

#[tokio::test]
async fn signal_shutdown_finishes_initialized_tasks() {
    for initialized in [false, true] {
        let (initialized_tx, initialized_rx) = tokio::sync::oneshot::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel();
        let finished = std::cell::Cell::new(false);
        let start = async {
            // Keep the readiness sender alive during incomplete startup.
            let _initialized_tx = if initialized {
                initialized_tx.send(()).unwrap();
                None
            } else {
                Some(initialized_tx)
            };
            signal_tx.send(()).unwrap();
            shutdown_rx.await.unwrap();
            // Cleanup can suspend; it must be awaited, not merely polled once.
            tokio::task::yield_now().await;
            finished.set(true);
            Ok(())
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            run_until_shutdown(
                start,
                async { signal_rx.await.unwrap() },
                initialized_rx,
                shutdown_tx,
            ),
        )
        .await
        .expect("shutdown must not wait for incomplete startup")
        .unwrap();
        assert_eq!(finished.get(), initialized);
    }
}
