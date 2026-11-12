//! Tests for the script verifier service.

use std::sync::Arc;

use tower::ServiceExt;
use zebra_chain::{
    block::Block,
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserialize,
    transaction::{self, Transaction, TransparentSigningKey},
    transparent,
};
use zebra_script::CachedFfiTransaction;

use super::{Request, Verifier, VERIFIER};

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
        err.to_string().contains("input index out of bounds"),
        "unexpected error message: {err}",
    );
}

/// Returns a real signed transaction for `upgrade` that spends two pay-to-public-key-hash outputs,
/// and those outputs.
fn signed_spend(upgrade: NetworkUpgrade) -> (Transaction, Vec<transparent::Output>) {
    let network = Network::Mainnet;
    let key = TransparentSigningKey::random();
    let lock_script = key.address(network.kind()).script();
    let spent: Vec<_> = [100_000_000u64, 200_000_000]
        .into_iter()
        .map(|value| {
            transparent::Output::new(value.try_into().expect("valid amount"), lock_script.clone())
        })
        .collect();
    let inputs: Vec<_> = spent
        .iter()
        .zip(0..)
        .map(|(output, index)| {
            let outpoint = transparent::OutPoint {
                hash: transaction::Hash([1; 32]),
                index,
            };
            (outpoint, output.clone())
        })
        .collect();
    let height = upgrade
        .activation_height(&network)
        .expect("the upgrade activates on Mainnet");
    let tx = Transaction::signed_transparent(&network, height, &key, &inputs, &[])
        .expect("the spent outputs pay the fee");

    (tx, spent)
}

/// Returns `tx` with each input's unlock script copied from the input at the same position in
/// `sources`.
///
/// Sighashes do not commit to unlock scripts, so a copied signature stays valid only at the input
/// it was made for, and the transaction ID only changes for v4 transactions.
fn with_unlock_scripts_from(tx: &Transaction, sources: [usize; 2]) -> Transaction {
    let unlock_scripts: Vec<_> = tx
        .inputs()
        .into_iter()
        .map(|input| match input {
            transparent::Input::PrevOut { unlock_script, .. } => unlock_script,
            transparent::Input::Coinbase { .. } => panic!("the fixture spends previous outputs"),
        })
        .collect();
    let mut inputs = tx.inputs();
    for (input, source) in inputs.iter_mut().zip(sources) {
        if let transparent::Input::PrevOut { unlock_script, .. } = input {
            *unlock_script = unlock_scripts[source].clone();
        }
    }

    tx.clone().with_transparent_inputs(inputs)
}

/// Returns a request for `input_index` of a newly prepared `tx` spending `spent` under `nu`.
fn request(
    tx: &Transaction,
    spent: &[transparent::Output],
    nu: NetworkUpgrade,
    input_index: usize,
) -> Request {
    Request {
        cached_ffi_transaction: Arc::new(
            CachedFfiTransaction::new(Arc::new(tx.clone()), Arc::new(spent.to_vec()), nu)
                .expect("the transaction and network upgrade are consistent"),
        ),
        input_index,
    }
}

/// A remembered script success answers only the same transaction, spent outputs and input.
///
/// A private cache wraps the real script verifier, which rejects every variant below, so a key
/// that left out what a variant changes would accept it.
#[tokio::test]
async fn cached_script_success_needs_the_same_transaction_spent_outputs_and_input() {
    let _init_guard = zebra_test::init();

    let nu = NetworkUpgrade::Nu5;
    let (signed, spent) = signed_spend(nu);
    // Only input 0 is valid. The twin has the same transaction ID, but its input 0 carries input
    // 1's signature.
    let tx = with_unlock_scripts_from(&signed, [0, 0]);
    let twin = with_unlock_scripts_from(&signed, [1, 0]);
    assert_eq!(twin.hash(), tx.hash());
    assert_ne!(twin.unmined_id(), tx.unmined_id());

    let cache = VERIFIER.isolated();
    for _ in 0..2 {
        cache
            .clone()
            .oneshot(request(&tx, &spent, nu, 0))
            .await
            .expect("input 0 is signed");
    }
    assert_eq!(
        cache.inner_calls_for(&request(&tx, &spent, nu, 0)),
        1,
        "a separately prepared copy must be answered from the cache"
    );

    let mut amount = spent.clone();
    amount[0].value = 300_000_000u64.try_into().expect("valid amount");
    let mut script = spent.clone();
    script[0].lock_script = TransparentSigningKey::random()
        .address(Network::Mainnet.kind())
        .script();
    // V5 `SIGHASH_ALL` signatures commit to every input's spent output.
    let mut other_input = spent.clone();
    other_input[1].value = 300_000_000u64.try_into().expect("valid amount");

    for (variant, tx, spent, input_index) in [
        ("authorizing data", &twin, &spent, 0),
        ("spent amount", &tx, &amount, 0),
        ("spent script", &tx, &script, 0),
        ("another input's spent output", &tx, &other_input, 0),
        ("input index", &tx, &spent, 1),
    ] {
        for attempt in 1..=2 {
            cache
                .clone()
                .oneshot(request(tx, spent, nu, input_index))
                .await
                .expect_err("the real script verifier rejects every variant");
            assert_eq!(
                cache.inner_calls_for(&request(tx, spent, nu, input_index)),
                attempt,
                "{variant}: every failure must reach the script verifier"
            );
        }
    }

    // Spent outputs that don't match the inputs are rejected instead of panicking.
    let mut extra = spent.clone();
    extra.push(spent[0].clone());
    cache
        .oneshot(request(&tx, &extra, nu, 0))
        .await
        .expect_err("spent outputs must match the inputs");
}

/// A v4 signature commits to the consensus branch, which a v4 transaction ID does not.
#[tokio::test]
async fn cached_v4_script_success_needs_the_same_consensus_branch() {
    let _init_guard = zebra_test::init();

    let (tx, spent) = signed_spend(NetworkUpgrade::Canopy);
    let cache = VERIFIER.isolated();

    cache
        .clone()
        .oneshot(request(&tx, &spent, NetworkUpgrade::Canopy, 0))
        .await
        .expect("input 0 is signed under Canopy");
    cache
        .clone()
        .oneshot(request(&tx, &spent, NetworkUpgrade::Nu5, 0))
        .await
        .expect_err("the signature does not commit to the NU5 branch");
    assert_eq!(
        cache.inner_calls_for(&request(&tx, &spent, NetworkUpgrade::Nu5, 0)),
        1,
        "the other branch must reach the script verifier"
    );
}
