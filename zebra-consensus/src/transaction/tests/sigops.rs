//! Tests for the mempool's total signature-operation standardness limit.

use std::sync::Arc;

use tokio::time::timeout;
use tower::{service_fn, ServiceExt};

use zebra_chain::{
    amount::Amount,
    parameters::{Network, NetworkUpgrade},
    transaction::{Hash, LockTime, Transaction},
    transparent,
};
use zebra_state as zs;

use crate::error::TransactionError;

use super::{
    super::{MempoolRequest, MempoolTxVerifier},
    test_timeout,
};

/// Test the mempool's `MAX_STANDARD_TX_SIGOPS` limit on legacy plus P2SH sigops: a transaction
/// above the limit is rejected before script verification, and a transaction at the limit reaches
/// it. Every input is within the per-input `MAX_P2SH_SIGOPS` limit, so only the total catches it.
///
/// <https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-423j-2vj4-j5x4>
#[tokio::test]
async fn mempool_rejects_too_many_sigops_before_script_verification() {
    let _init_guard = zebra_test::init();

    const OP_CHECKSIG: u8 = 0xac;

    let network = Network::Mainnet;
    let height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("Nu5 activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");

    // A P2SH output whose hash doesn't match the redeem script, so its spends fail script
    // verification.
    let mut p2sh_lock_bytes = vec![0xa9, 0x14];
    p2sh_lock_bytes.extend_from_slice(&[0u8; 20]);
    p2sh_lock_bytes.push(0x87);
    let spent_utxo = transparent::Utxo::new(
        transparent::Output {
            value: Amount::try_from(1_000_000).expect("valid amount"),
            lock_script: transparent::Script::new(&p2sh_lock_bytes),
        },
        fund_height,
        false,
    );

    // 266 inputs whose redeem scripts each have `MAX_P2SH_SIGOPS` sigops: 3,990 P2SH sigops.
    let mut unlock_bytes = vec![15];
    unlock_bytes.extend_from_slice(&[OP_CHECKSIG; 15]);
    let outpoints: Vec<_> = (0..266)
        .map(|index| transparent::OutPoint {
            hash: Hash([1; 32]),
            index,
        })
        .collect();
    let inputs: Vec<_> = outpoints
        .iter()
        .map(|&outpoint| transparent::Input::PrevOut {
            outpoint,
            unlock_script: transparent::Script::new(&unlock_bytes),
            sequence: u32::MAX,
        })
        .collect();

    // The output's legacy sigops bring the total to the limit, or one above it.
    for legacy_sigops in [10, 11] {
        let tx = Transaction::test_v5(
            NetworkUpgrade::Nu5,
            inputs.clone(),
            vec![transparent::Output {
                value: Amount::try_from(1).expect("valid amount"),
                lock_script: transparent::Script::new(&vec![OP_CHECKSIG; legacy_sigops]),
            }],
            LockTime::unlocked(),
            height,
        );

        let spent_utxo = spent_utxo.clone();
        let state = service_fn(move |request: zs::ReadRequest| {
            let spent_utxo = spent_utxo.clone();
            async move {
                match request {
                    zs::ReadRequest::UnspentBestChainUtxo(outpoint) => {
                        assert_eq!(outpoint.hash, Hash([1; 32]));
                        assert!(outpoint.index < 266);
                        Ok(zs::ReadResponse::UnspentBestChainUtxo(Some(spent_utxo)))
                    }
                    zs::ReadRequest::CheckBestChainTipNullifiersAndAnchors(_) => {
                        assert_eq!(
                            legacy_sigops, 10,
                            "over-limit transactions never dispatch checks"
                        );
                        Ok(zs::ReadResponse::ValidBestChainTipNullifiersAndAnchors)
                    }
                    other => Err(format!("unexpected state request: {other:?}").into()),
                }
            }
        });
        let verifier = MempoolTxVerifier::new_for_tests(&network, state);

        let result = timeout(
            test_timeout(),
            verifier.oneshot(MempoolRequest {
                transaction: Arc::new(tx).into(),
                height,
            }),
        )
        .await
        .expect("verification should not time out")
        .map(|_| ());

        if legacy_sigops == 10 {
            assert!(
                matches!(result, Err(TransactionError::Script(_))),
                "a transaction at the limit should reach script verification, got {result:?}"
            );
        } else {
            let error = TransactionError::NonStandardSigops { sigops: 4001 };
            assert_eq!(
                result,
                Err(error.clone()),
                "a transaction above the limit should be rejected before script verification"
            );
            // zcashd rejects it with `DoS(0, ...)`: standardness rejections don't score the peer.
            assert_eq!(error.mempool_misbehavior_score(), 0);
        }
    }
}
