//! Accepted commits remain supervised after their requesting future is cancelled.

use super::*;

fn genesis() -> Arc<Block> {
    Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]).unwrap()
}

#[tokio::test]
async fn cancelled_checkpoint_commit_panics_reach_owner() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        for failure in 0..3 {
            let block = genesis();
            let (release, released) = tokio::sync::watch::channel(false);
            let (started, mut starts) = tokio::sync::mpsc::channel(1);
            let state = tower::service_fn(move |request| {
                let mut released = released.clone();
                let started = started.clone();
                async move {
                    assert!(matches!(
                        request,
                        zs::Request::CommitCheckpointVerifiedBlock(_)
                    ));
                    started.send(()).await.unwrap();
                    released.wait_for(|released| *released).await.unwrap();
                    match failure {
                        0 => panic!("state commit task panic"),
                        1 => Ok::<_, BoxError>(zs::Response::Committed(block::Hash([7; 32]))),
                        _ => Ok(zs::Response::Tip(None)),
                    }
                }
            });
            let mut verifier = CheckpointVerifier::from_list(
                [(block::Height(0), block.hash())],
                &Mainnet,
                None,
                state,
            )
            .unwrap();
            let owner = verifier.commit_task_handle.take().unwrap();
            drop(verifier.call(block));
            starts.recv().await.unwrap();
            release.send(true).unwrap();
            let error = owner
                .await
                .expect_err("the owner must observe every commit-task panic");
            assert!(error.is_panic());
            let payload = error.into_panic();
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .expect("the commit invariant uses a string panic payload");
            let expected = match failure {
                0 => "state commit task panic",
                1 => "state must commit correct hash",
                _ => "wrong response for CommitCheckpointVerifiedBlock",
            };
            assert!(message.contains(expected), "unexpected panic: {message}");
        }
    })
    .await
    .expect("commit panics must reach the owner without another verifier request");
}

#[tokio::test]
async fn accepted_checkpoint_commit_outlives_caller_and_verifier() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let block = genesis();
        let hash = block.hash();
        let body = Arc::downgrade(&block);
        let (release, released) = tokio::sync::watch::channel(false);
        let (started, mut starts) = tokio::sync::mpsc::channel(1);
        let (committed, mut result) = tokio::sync::mpsc::channel(1);
        let state = tower::service_fn(move |request| {
            let mut released = released.clone();
            let started = started.clone();
            let committed = committed.clone();
            async move {
                let zs::Request::CommitCheckpointVerifiedBlock(block) = request else {
                    panic!("only the accepted commit is expected")
                };
                started.send(()).await.unwrap();
                released.wait_for(|released| *released).await.unwrap();
                committed.send(block.hash).await.unwrap();
                Ok::<_, BoxError>(zs::Response::Committed(block.hash))
            }
        });
        let mut verifier =
            CheckpointVerifier::from_list([(block::Height(0), hash)], &Mainnet, None, state)
                .unwrap();
        let owner = verifier.commit_task_handle.take().unwrap();
        let retained = verifier.retained_blocks.clone();
        drop(verifier.call(block));
        starts.recv().await.unwrap();
        drop(verifier);
        assert_eq!(retained.load(Ordering::Relaxed), 1);
        release.send(true).unwrap();
        assert_eq!(result.recv().await.unwrap(), hash);
        owner
            .await
            .expect("supervision must drain accepted commits before finishing");
        assert_eq!(retained.load(Ordering::Relaxed), 0);
        assert_eq!(body.strong_count(), 0);
    })
    .await
    .expect("accepted commits must survive caller and verifier cancellation");
}
