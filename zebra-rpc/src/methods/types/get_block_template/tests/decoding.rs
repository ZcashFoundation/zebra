//! Large decoding work owns admission even after its requesting task is cancelled.

use std::time::Duration;

use super::super::{decode_block, BLOCK_DECODES, BLOCK_DECODE_LIMIT};

#[test]
fn cancelled_large_decode_retains_admission_until_blocking_work_finishes() {
    let _init_guard = zebra_test::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut reserved = BLOCK_DECODES
                .acquire_many(u32::try_from(BLOCK_DECODE_LIMIT).unwrap())
                .await
                .unwrap();
            drop(reserved.split(1).unwrap());
            let (started, running) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let blocking = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(30)).unwrap();
            });
            running.await.unwrap();

            let decoding = tokio::spawn(decode_block(vec![0; 4 * 1024 + 1], None));
            while BLOCK_DECODES.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
            decoding.abort();
            assert!(decoding.await.unwrap_err().is_cancelled());
            assert!(BLOCK_DECODES.try_acquire().is_err());

            release.send(()).unwrap();
            blocking.await.unwrap();
            let released = BLOCK_DECODES.acquire().await.unwrap();
            drop(released);
            drop(reserved);
        })
        .await
        .expect("cancellation must not detach decode admission from queued blocking work");
    });
}
