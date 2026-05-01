//! Tests for the script verifier service.

use std::sync::Arc;

use tower::ServiceExt;
use zebra_chain::{
    block::Block, parameters::NetworkUpgrade, serialization::ZcashDeserialize, transparent,
};
use zebra_script::CachedFfiTransaction;

use super::{Request, Verifier};

/// Calling the script `Verifier` with an `input_index` past the end of the transaction's
/// inputs must return an error instead of panicking.
#[tokio::test]
async fn verifier_returns_error_for_out_of_range_input_index() {
    let _init_guard = zebra_test::init();

    // Use a v4 mainnet block whose second transaction has at least one transparent input.
    let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_419199_BYTES[..])
        .expect("test vector is a valid block");
    let tx = block
        .transactions
        .iter()
        .find(|tx| {
            tx.inputs()
                .iter()
                .any(|i| matches!(i, transparent::Input::PrevOut { .. }))
        })
        .expect("block has a non-coinbase tx")
        .clone();

    // Build matching previous outputs (one per input). Their content does not matter
    // because the verifier should error on the out-of-range index before touching them.
    let previous_outputs: Vec<transparent::Output> = tx
        .inputs()
        .iter()
        .map(|_| transparent::Output {
            value: 0.try_into().expect("zero is a valid amount"),
            lock_script: transparent::Script::new(&[]),
        })
        .collect();

    let cached = Arc::new(
        CachedFfiTransaction::new(
            tx.clone(),
            Arc::new(previous_outputs),
            NetworkUpgrade::Sapling,
        )
        .expect("constructor accepts the test fixture"),
    );

    let out_of_range = tx.inputs().len();
    let result = Verifier
        .oneshot(Request {
            cached_ffi_transaction: cached,
            input_index: out_of_range,
        })
        .await;

    let err = result.expect_err("out-of-range input_index must error, not panic");
    assert!(
        err.to_string().contains("missing input"),
        "unexpected error message: {err}",
    );
}
