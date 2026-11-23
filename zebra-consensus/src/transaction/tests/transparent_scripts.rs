//! Tests for the misbehaviour score of mempool transactions whose transparent scripts fail.
//!
//! See <https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-jq45-ghw5-v37x>.

use std::{collections::HashMap, sync::Arc};

use tower::{service_fn, util::BoxCloneService, ServiceExt};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block,
    parameters::{Network, NetworkUpgrade},
    transaction::{self, LockTime, Transaction},
    transparent,
};
use zebra_state as zs;

use crate::{error::TransactionError, BoxError};

use super::{
    super::{MempoolRequest, MempoolTxVerifier},
    test_timeout,
};

/// A strictly DER-encoded ECDSA signature with r = 1 and s = 1, followed by `SIGHASH_ALL`.
///
/// It passes the signature encoding checks, so the interpreter computes the signature hash, then
/// fails the ECDSA check. No private key is needed to create it.
const STUB_SIGNATURE: [u8; 9] = [0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x01];

/// The `OP_CHECKSIG` opcode.
const OP_CHECKSIG: u8 = 0xac;

/// The value of each spent output, 0.01 ZEC.
const VALUE_PER_INPUT: u64 = 1_000_000;

/// The marginal ZIP-317 fee per logical action.
const MARGINAL_FEE: u64 = 5_000;

#[derive(Clone, Copy, Debug)]
enum Version {
    V4,
    V5,
}

/// The test network.
fn network() -> Network {
    Network::Mainnet
}

/// The height the test transactions are verified at, while NU6 is active.
fn height() -> block::Height {
    let nu6 = NetworkUpgrade::Nu6
        .activation_height(&network())
        .expect("NU6 is active on Mainnet");

    (nu6 + 10).expect("the height is valid")
}

/// The compressed secp256k1 generator point, a valid public key.
///
/// The test signatures never verify, so any valid public key works.
const PUBLIC_KEY: [u8; 33] = [
    0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
    0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8, 0x17,
    0x98,
];

/// A standard pay-to-public-key `scriptPubKey` for [`PUBLIC_KEY`].
fn p2pk_lock_script() -> transparent::Script {
    let mut script = vec![u8::try_from(PUBLIC_KEY.len()).expect("public keys are 33 bytes")];
    script.extend_from_slice(&PUBLIC_KEY);
    script.push(OP_CHECKSIG);

    transparent::Script::new(&script)
}

/// A pay-to-public-key `scriptSig` that pushes `signature`.
fn p2pk_unlock_script(signature: &[u8]) -> transparent::Script {
    let mut script = vec![u8::try_from(signature.len()).expect("signatures are short pushes")];
    script.extend_from_slice(signature);

    transparent::Script::new(&script)
}

/// The output spent by each test input.
fn spent_output() -> transparent::Output {
    transparent::Output {
        value: VALUE_PER_INPUT.try_into().expect("the value is valid"),
        lock_script: p2pk_lock_script(),
    }
}

/// The outpoint of the `index`th spent output.
fn outpoint(index: usize) -> transparent::OutPoint {
    let mut hash = [0; 32];
    hash[..8].copy_from_slice(
        &u64::try_from(index)
            .expect("usize fits in u64")
            .to_le_bytes(),
    );

    transparent::OutPoint {
        hash: transaction::Hash(hash),
        index: 0,
    }
}

/// A transaction of `version` that spends the first `input_count` test outputs with `script_sig`,
/// and pays a fee above the ZIP-317 conventional fee.
fn spend(version: Version, input_count: usize, script_sig: &transparent::Script) -> Transaction {
    let inputs = (0..input_count)
        .map(|index| transparent::Input::PrevOut {
            outpoint: outpoint(index),
            unlock_script: script_sig.clone(),
            sequence: u32::MAX,
        })
        .collect();

    let input_count = u64::try_from(input_count).expect("usize fits in u64");
    let fee = MARGINAL_FEE * input_count.max(2);
    let value: Amount<NonNegative> = (VALUE_PER_INPUT * input_count - fee)
        .try_into()
        .expect("the value is valid");
    let outputs = vec![transparent::Output {
        value,
        lock_script: p2pk_lock_script(),
    }];

    match version {
        Version::V4 => {
            Transaction::test_v4(inputs, outputs, LockTime::unlocked(), block::Height(0))
        }
        Version::V5 => Transaction::test_v5(
            NetworkUpgrade::Nu6,
            inputs,
            outputs,
            LockTime::unlocked(),
            block::Height(0),
        ),
    }
}

/// A state service that holds the first `input_count` test outputs.
fn state(input_count: usize) -> BoxCloneService<zs::Request, zs::Response, BoxError> {
    let utxo_height = (height() - 1_000).expect("the height is valid");
    let utxos: Arc<HashMap<_, _>> = Arc::new(
        (0..input_count)
            .map(|index| {
                (
                    outpoint(index),
                    transparent::Utxo::new(spent_output(), utxo_height, false),
                )
            })
            .collect(),
    );

    BoxCloneService::new(service_fn(move |request: zs::Request| {
        let utxos = utxos.clone();

        async move {
            match request {
                zs::Request::UnspentBestChainUtxo(outpoint) => Ok(
                    zs::Response::UnspentBestChainUtxo(utxos.get(&outpoint).cloned()),
                ),
                zs::Request::CheckBestChainTipNullifiersAndAnchors(_) => {
                    Ok(zs::Response::ValidBestChainTipNullifiersAndAnchors)
                }
                other => Err(format!("unexpected state request: {other:?}").into()),
            }
        }
    }))
}

/// Verifies `transaction` for the mempool, with a state that holds the outputs it spends.
async fn verify_for_mempool(transaction: Arc<Transaction>) -> Result<(), TransactionError> {
    let state = state(transaction.inputs().len());

    tokio::time::timeout(
        test_timeout(),
        MempoolTxVerifier::new_for_tests(&network(), state).oneshot(MempoolRequest {
            transaction: transaction.into(),
            height: height(),
        }),
    )
    .await
    .expect("mempool verification finishes")
    .map(|_response| ())
}

/// A transaction with failing transparent scripts is rejected with a misbehaviour score that
/// bans the peer that sent it.
///
/// Before GHSA-jq45-ghw5-v37x was fixed, it was rejected with an unscored
/// `InternalDowncastError`.
#[tokio::test(flavor = "multi_thread")]
async fn failed_transparent_scripts_are_scored() {
    let _init_guard = zebra_test::init();

    const INPUTS: usize = 2_000;

    for version in [Version::V4, Version::V5] {
        let transaction = Arc::new(spend(version, INPUTS, &p2pk_unlock_script(&STUB_SIGNATURE)));

        let error = verify_for_mempool(transaction)
            .await
            .expect_err("every signature in the transaction is invalid");

        assert_eq!(
            error,
            TransactionError::Script(zebra_script::Error::ScriptInvalid),
            "{version:?}"
        );
        assert_eq!(error.mempool_misbehavior_score(), 100, "{version:?}");
    }
}
