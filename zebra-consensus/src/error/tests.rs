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
    ] {
        assert_eq!(error.mempool_misbehavior_score(), 100, "{error:?}");
    }
}
