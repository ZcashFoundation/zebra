//! Inbound service tests.

mod fake_peer_set;
mod real_peer_set;

use zebra_chain::parameters::subsidy::SubsidyError;
use zebra_consensus::{router::RouterError, VerifyBlockError};

use crate::BoxError;

use super::block_download_misbehavior_score;

/// A `VerifyBlockError` whose `misbehavior_score()` is 100.
fn score_100_block_error() -> VerifyBlockError {
    VerifyBlockError::Subsidy(SubsidyError::NoCoinbase)
}

/// Regression test for #10616: an inbound block-download error that is a
/// `RouterError` (what the consensus router returns) must still yield its
/// misbehavior score. Before the fix the code downcast only to
/// `VerifyBlockError`, so router failures scored zero and gossiped invalid
/// blocks went unpenalised.
#[test]
fn router_error_yields_misbehavior_score() {
    let router_err: RouterError = score_100_block_error().into();
    assert_eq!(router_err.misbehavior_score(), 100);

    let boxed: BoxError = Box::new(router_err);
    assert_eq!(block_download_misbehavior_score(boxed), 100);
}

/// A `VerifyBlockError` surfaced directly is still scored (fallback path).
#[test]
fn verify_block_error_yields_misbehavior_score() {
    let boxed: BoxError = Box::new(score_100_block_error());
    assert_eq!(block_download_misbehavior_score(boxed), 100);
}

/// An unrelated error scores zero.
#[test]
fn unrelated_error_scores_zero() {
    let boxed: BoxError = "some unrelated error".into();
    assert_eq!(block_download_misbehavior_score(boxed), 0);
}
