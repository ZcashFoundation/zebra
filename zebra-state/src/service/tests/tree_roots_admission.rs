//! Cancelled responses must not free admission for detached tree-root database work.

use std::time::Duration;

use futures::FutureExt;
use tokio::sync::oneshot;
use tower::{Service, ServiceExt};
use zebra_chain::{block::Height, parameters::Network};

use crate::{service::StateService, Config, ReadRequest, ReadResponse};

fn tree_roots_request() -> ReadRequest {
    ReadRequest::TreeRoots {
        start_height: Height(0),
        final_hash: [0; 32].into(),
        count: 1,
    }
}

#[test]
fn cancelled_tree_root_reads_retain_blocking_admission() {
    let _init_guard = zebra_test::init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let (_state, mut read, _, _) =
                StateService::new(Config::ephemeral(), &Network::Mainnet, Height(0), 0).await;
            let (release, released) = oneshot::channel::<()>();
            let (started, running) = oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = released.blocking_recv();
            });
            running.await.unwrap();

            for _ in 0..3 {
                drop(read.ready().await.unwrap().call(tree_roots_request()));
            }
            assert!(matches!(
                read.ready()
                    .await
                    .unwrap()
                    .call(tree_roots_request())
                    .now_or_never(),
                Some(Err(_))
            ));

            release.send(()).unwrap();
            blocker.await.unwrap();
            let response = loop {
                match read.clone().oneshot(tree_roots_request()).await {
                    Ok(response) => break response,
                    Err(_) => tokio::task::yield_now().await,
                }
            };
            assert_eq!(response, ReadResponse::TreeRoots(None));
        })
        .await
        .expect("tree-root admission must recover after detached reads finish");
    });
}
