//! Tests for [`TransactionError`] conversion and mempool misbehaviour scoring.

use zebra_chain::{parameters::NetworkUpgrade, transaction};
use zebra_state::DuplicateNullifierError;

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
    ] {
        assert_eq!(error.mempool_misbehavior_score(), 100, "{error:?}");
    }
}

/// Structural consensus rules are invalid on every node and match zcashd's score of 100.
/// A peer that keeps sending them should be banned.
#[test]
fn structural_consensus_errors_have_high_misbehavior_score() {
    let outpoint = transparent::OutPoint {
        hash: transaction::Hash([0; 32]),
        index: 0,
    };
    let orchard_nullifier =
        orchard::Nullifier::try_from([0u8; 32]).expect("zero is a canonical field element");
    let ironwood_nullifier = ironwood::Nullifier::from(orchard_nullifier);

    for error in [
        TransactionError::DuplicateTransparentSpend(outpoint),
        TransactionError::DuplicateSproutNullifier(sprout::Nullifier([0; 32].into())),
        TransactionError::DuplicateSaplingNullifier(sapling::Nullifier([0; 32].into())),
        TransactionError::DuplicateOrchardNullifier(orchard_nullifier),
        TransactionError::DuplicateIronwoodNullifier(ironwood_nullifier),
        TransactionError::MaximumExpiryHeight {
            expiry_height: block::Height::MAX,
            is_coinbase: false,
            block_height: block::Height(1),
            transaction_hash: transaction::Hash([0; 32]),
        },
    ] {
        assert_eq!(error.mempool_misbehavior_score(), 100, "{error:?}");
    }
}

/// Policy rejections and errors that depend on the receiving node's view should not be punished,
/// because honest relays can trigger them.
#[test]
fn policy_and_context_errors_have_zero_misbehavior_score() {
    let orchard_nullifier =
        orchard::Nullifier::try_from([0u8; 32]).expect("zero is a canonical field element");

    for error in [
        TransactionError::NonStandardScriptSigSize {
            input_index: 0,
            size: 0,
        },
        TransactionError::NonStandardScriptSigNotPushOnly { input_index: 0 },
        TransactionError::NonStandardInputs,
        TransactionError::TooManyShieldedActions {
            pool: "Orchard actions",
            count: 1,
            limit: 0,
        },
        TransactionError::TransparentInputNotFound,
        // ValidateContextError is #[non_exhaustive], so build one through the public DuplicateNullifierError trait.
        TransactionError::from(orchard_nullifier.duplicate_nullifier_error(false)),
        TransactionError::ValidateMempoolLockTimeError(String::new()),
        TransactionError::UnsupportedByNetworkUpgrade(5, NetworkUpgrade::Nu5),
        TransactionError::ExpiredTransaction {
            expiry_height: block::Height(1),
            block_height: block::Height(2),
            transaction_hash: transaction::Hash([0; 32]),
        },
    ] {
        assert_eq!(error.mempool_misbehavior_score(), 0, "{error:?}");
    }
}
