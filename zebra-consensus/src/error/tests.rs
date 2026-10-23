//! Tests for [`TransactionError`] conversion and mempool misbehaviour scoring.

use super::*;

/// Boxed shielded proof and signature verification errors must keep their concrete type when
/// converted back from a [`BoxError`], so the mempool can assign them a misbehaviour score instead
/// of collapsing them to [`TransactionError::InternalDowncastError`] (score 0). See
/// <https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-2p4c-3q4q-p463>.
#[test]
fn boxed_signature_errors_are_preserved() {
    let ed25519_error: BoxError =
        Box::new(zebra_chain::primitives::ed25519::Error::InvalidSignature);
    assert_eq!(
        TransactionError::from(ed25519_error),
        TransactionError::Ed25519(zebra_chain::primitives::ed25519::Error::InvalidSignature)
    );

    let redjubjub_error: BoxError =
        Box::new(zebra_chain::primitives::redjubjub::Error::InvalidSignature);
    assert_eq!(
        TransactionError::from(redjubjub_error),
        TransactionError::RedJubjub(zebra_chain::primitives::redjubjub::Error::InvalidSignature)
    );

    let redpallas_error: BoxError =
        Box::new(zebra_chain::primitives::reddsa::Error::InvalidSignature);
    assert_eq!(
        TransactionError::from(redpallas_error),
        TransactionError::RedPallas(zebra_chain::primitives::reddsa::Error::InvalidSignature)
    );
}

/// Boxed transparent script verification errors must keep their type when converted back from a
/// [`BoxError`], so that the existing `TransactionError::Script` misbehaviour score applies. See
/// <https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-jq45-ghw5-v37x>.
#[test]
fn boxed_script_errors_are_preserved() {
    let script_error: BoxError = Box::new(zebra_script::Error::ScriptInvalid);
    assert_eq!(
        TransactionError::from(script_error),
        TransactionError::Script(zebra_script::Error::ScriptInvalid)
    );
}

/// Every shielded proof/signature verification failure, and non-canonical Orchard/Ironwood proof
/// sizes, must earn a ban-worthy mempool misbehaviour score, so a peer forcing expensive
/// verification with invalid proofs is disconnected rather than allowed to keep sending. See
/// <https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-2p4c-3q4q-p463>.
#[test]
fn verification_errors_have_high_misbehavior_score() {
    for error in [
        TransactionError::SaplingVerificationFailed,
        TransactionError::Halo2VerificationFailed,
        TransactionError::OrchardProofSize,
        TransactionError::IronwoodProofSize,
        TransactionError::Ed25519(zebra_chain::primitives::ed25519::Error::InvalidSignature),
        TransactionError::RedJubjub(zebra_chain::primitives::redjubjub::Error::InvalidSignature),
        TransactionError::RedPallas(zebra_chain::primitives::reddsa::Error::InvalidSignature),
        TransactionError::Script(zebra_script::Error::ScriptInvalid),
        TransactionError::Subsidy(SubsidyError::InvalidMinerFees),
    ] {
        assert_eq!(error.mempool_misbehavior_score(), 100, "{error:?}");
        assert!(error.is_deterministic_for_block(), "{error:?}");
    }
}

#[test]
fn early_relay_rejection_excludes_transient_and_unclassified_errors() {
    for error in [
        TransactionError::TransparentInputNotFound,
        TransactionError::LockedUntilAfterBlockHeight(block::Height(10)),
        TransactionError::LockedUntilAfterBlockTime(Utc::now()),
        TransactionError::ExpiredTransaction {
            expiry_height: block::Height(1),
            block_height: block::Height(2),
            transaction_hash: [0; 32].into(),
        },
        TransactionError::ImmatureTransparentCoinbaseSpend {
            outpoint: transparent::OutPoint {
                hash: [0; 32].into(),
                index: 0,
            },
            spend_height: block::Height(2),
            min_spend_height: block::Height(101),
            created_height: block::Height(1),
        },
        TransactionError::ValidateMempoolLockTimeError("state unavailable".into()),
        TransactionError::Subsidy(SubsidyError::ParentChainValuePoolsRequired(block::Height(
            10,
        ))),
        TransactionError::InternalDowncastError("worker unavailable".into()),
        TransactionError::Io("disk unavailable".into()),
        TransactionError::Other("unknown failure".into()),
        TransactionError::Amount("unclassified amount error".into()),
        TransactionError::Balance("unclassified balance error".into()),
        TransactionError::Script(zebra_script::Error::TxIndex),
        TransactionError::Script(zebra_script::Error::TxCoinbase),
    ] {
        assert!(!error.is_deterministic_for_block(), "{error:?}");
    }
}
