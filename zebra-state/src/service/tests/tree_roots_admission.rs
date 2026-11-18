//! Cancelled reads retain admission, and saturated tree-root reads return a neutral refusal.

use std::{sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::sync::oneshot;
use tower::{Service, ServiceExt};
use zebra_chain::{
    block::{Block, Height},
    parallel::tree::{TreeRoots, TreeRootsProof},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use crate::{service::StateService, Config, ReadRequest, ReadResponse};

fn tree_roots_request() -> ReadRequest {
    ReadRequest::TreeRoots {
        start_height: Height(0),
        final_hash: Network::Mainnet.genesis_hash(),
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
            let (mut state, mut read, _, _) =
                StateService::new(Config::ephemeral(), &Network::Mainnet, Height(0), 0).await;
            let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
                .zcash_deserialize_into::<Arc<Block>>()
                .unwrap();
            state
                .queue_and_commit_to_finalized_state(genesis.into(), None)
                .await
                .unwrap()
                .unwrap();
            let expected = ReadResponse::TreeRoots(Some(vec![TreeRootsProof {
                roots: TreeRoots::empty(),
                successor_auth_data_root: None,
            }]));
            assert_eq!(
                read.ready()
                    .await
                    .unwrap()
                    .call(tree_roots_request())
                    .await
                    .unwrap(),
                expected
            );
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
                Some(Ok(ReadResponse::TreeRoots(None)))
            ));

            release.send(()).unwrap();
            blocker.await.unwrap();
            let response = loop {
                match read.clone().oneshot(tree_roots_request()).await.unwrap() {
                    ReadResponse::TreeRoots(None) => tokio::task::yield_now().await,
                    response => break response,
                }
            };
            assert_eq!(response, expected);
        })
        .await
        .expect("tree-root admission must recover after detached reads finish");
    });
}
