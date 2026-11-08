//! Transaction verification must start before dispatch finishes the whole block.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use tower::{service_fn, ServiceExt};
use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserialize};

use crate::{
    block::{Request, SemanticBlockVerifier},
    error::TransactionError,
    transaction::{BlockRequest, BlockResponse},
    BoxError,
};

#[tokio::test]
async fn block_transaction_dispatch_interleaves_verification() {
    let _init_guard = zebra_test::init();
    let block = Arc::new(
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES[..]).unwrap(),
    );
    let transaction_count = block.transactions.len();
    assert!(transaction_count > 1);

    let calls = Arc::new(AtomicUsize::new(0));
    let first_polled = Arc::new(AtomicBool::new(false));
    let transactions = service_fn({
        let calls = calls.clone();
        let first_polled = first_polled.clone();
        move |_: BlockRequest| {
            calls.fetch_add(1, Ordering::SeqCst);
            let calls = calls.clone();
            let first_polled = first_polled.clone();
            async move {
                if !first_polled.swap(true, Ordering::SeqCst) {
                    assert!(
                        calls.load(Ordering::SeqCst) < transaction_count,
                        "verification must be polled before every transaction is dispatched"
                    );
                }
                Err::<BlockResponse, BoxError>(TransactionError::WrongVersion.into())
            }
        }
    });
    let state = service_fn(|request| async move {
        assert!(matches!(request, zebra_state::Request::KnownBlock(_)));
        Ok::<_, BoxError>(zebra_state::Response::KnownBlock(None))
    });

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        SemanticBlockVerifier::new(&Network::Mainnet, state, transactions)
            .oneshot(Request::Commit(block)),
    )
    .await
    .expect("a ready transaction error must finish block verification");
    assert!(result.is_err());
    assert!(first_polled.load(Ordering::SeqCst));
}
