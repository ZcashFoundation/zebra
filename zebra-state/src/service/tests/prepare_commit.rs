//! The state answers a request to prepare a block's commit at once, whether or not the block
//! write task can prepare the block, and the block's commit request still commits it.

use std::{sync::Arc, time::Duration};

use tower::{Service, ServiceExt};

use crate::{arbitrary::Prepare, Request, Response};

use super::mining_rejection::{candidate, state};

#[tokio::test(flavor = "multi_thread")]
async fn prepare_commit_is_answered_for_any_block_and_its_commit_follows() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(true).await;

        // Before its first non-finalized block, the block write task prepares nothing. After it,
        // it prepares a child of a block it was sent, but not an orphan or a block it was sent.
        for _ in 0..3 {
            let block = candidate(&read, false, 0).await;
            let mut orphan = (*block).clone();
            Arc::make_mut(&mut orphan.header).previous_block_hash = block.hash();

            for prepared in [block.clone(), Arc::new(orphan)] {
                assert!(matches!(
                    state
                        .ready()
                        .await
                        .unwrap()
                        .call(Request::PrepareCommit(prepared))
                        .await
                        .unwrap(),
                    Response::PreparingCommit
                ));
            }

            assert_eq!(
                state
                    .ready()
                    .await
                    .unwrap()
                    .call(Request::CommitSemanticallyVerifiedBlock(
                        block.clone().prepare()
                    ))
                    .await
                    .unwrap(),
                Response::Committed(block.hash()),
            );
            assert!(matches!(
                state.ready().await.unwrap().call(Request::Tip).await.unwrap(),
                Response::Tip(Some((_, hash))) if hash == block.hash()
            ));

            assert!(matches!(
                state
                    .ready()
                    .await
                    .unwrap()
                    .call(Request::PrepareCommit(block))
                    .await
                    .unwrap(),
                Response::PreparingCommit
            ));
        }
    })
    .await
    .expect("prepare requests must not delay or prevent commits");
}
