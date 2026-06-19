//! Tests for the Halo2 Orchard Action verifier.
//!
//! The key correctness property of this module is the **era split**: the Orchard Action circuit
//! (and therefore its verifying key) changed at NU6.2 to fix a variable-base scalar-multiplication
//! soundness bug (GHSA-jfw5-j458-pfv6). A proof produced under one circuit does not verify under
//! the other key. These tests guard that:
//!
//!   * a real pre-NU6.2 Orchard proof verifies under the pre-NU6.2 (insecure) key, so historical
//!     blocks still re-sync;
//!   * the same proof is **rejected** by the post-NU6.2 (fixed) key, so the verifier is not
//!     "fail-open" — it does not accept whatever it is handed regardless of era; and
//!   * the Orchard verifier routing ([`orchard_v5_verifier_for`] / [`orchard_v6_verifier`])
//!     selects the matching key by transaction version and upgrade — in particular a v5 Orchard
//!     bundle at NU6.3 routes to the fixed key, not the NU6.3 cross-address key.

use std::sync::Arc;

use orchard::bundle::{Authorized, Bundle};
use zcash_protocol::value::ZatBalance;
use zebra_chain::{
    block::Block,
    parameters::NetworkUpgrade,
    serialization::ZcashDeserializeInto,
    transaction::{HashType, SigHash},
    transparent,
};

use super::{
    orchard_v5_verifier_for, orchard_v6_verifier, Item, VERIFIER_POST_NU6_2, VERIFIER_POST_NU6_3,
    VERIFIER_PRE_NU6_2, VERIFYING_KEY_POST_NU6_2, VERIFYING_KEY_PRE_NU6_2,
};

/// Returns one real pre-NU6.2 Orchard bundle and its sighash, extracted from the mainnet test
/// blocks.
///
/// These mainnet blocks are NU5-era Orchard history, mined long before NU6.2, so their proofs
/// were produced by the historical (insecure) circuit and only verify under
/// [`VERIFYING_KEY_PRE_NU6_2`]. Transactions with transparent inputs are skipped because their
/// sighash needs the previous outputs they spend, which are not in the test vectors.
fn pre_nu6_2_bundle_and_sighash() -> (Bundle<Authorized, ZatBalance>, SigHash) {
    for bytes in zebra_test::vectors::MAINNET_BLOCKS.values() {
        let block: Block = bytes
            .zcash_deserialize_into()
            .expect("hard-coded test vector must deserialize");

        for tx in &block.transactions {
            if tx.orchard_shielded_data().is_none() || !tx.inputs().is_empty() {
                continue;
            }

            let all_previous_outputs: Arc<Vec<transparent::Output>> = Arc::new(Vec::new());
            let Ok(sighasher) = tx.sighasher(NetworkUpgrade::Nu5, all_previous_outputs) else {
                continue;
            };
            let Some(bundle) = sighasher.orchard_bundle() else {
                continue;
            };

            let sighash = sighasher.sighash(HashType::ALL, None);
            return (bundle, sighash);
        }
    }

    panic!("mainnet test blocks must contain a transparent-input-free Orchard transaction");
}

/// A real pre-NU6.2 Orchard proof verifies under the pre-NU6.2 key and is rejected by the
/// post-NU6.2 key.
///
/// This is the core guard for the era split: it proves the two keys are genuinely different and
/// that selecting the wrong era's key causes a hard verification failure. If the verifier ever
/// "fails open" (e.g. validates everything against a single key, like the rejected zcashd WIP
/// shortcut), the wrong-key assertion below would fail.
#[test]
fn pre_nu6_2_proof_only_verifies_under_pre_nu6_2_key() {
    let (bundle, sighash) = pre_nu6_2_bundle_and_sighash();

    // Correct era key: the historical proof must verify, so pre-NU6.2 history still re-syncs.
    assert!(
        Item::new(bundle.clone(), sighash).verify_single(&VERIFYING_KEY_PRE_NU6_2),
        "a real pre-NU6.2 Orchard proof must verify under the pre-NU6.2 (insecure) key"
    );

    // Wrong era key: the same proof must be rejected. This is the not-fail-open guarantee.
    assert!(
        !Item::new(bundle, sighash).verify_single(&VERIFYING_KEY_POST_NU6_2),
        "a pre-NU6.2 Orchard proof must be REJECTED by the post-NU6.2 (fixed) key; \
         verifying it would mean the era selection is fail-open"
    );
}

/// The Orchard verifier routing selects the correct key by transaction version AND upgrade.
///
/// We compare service identity by pointer: the routing functions return a borrow of one of the
/// three global `Lazy` services, so routing to the wrong service is exactly routing to the wrong
/// key. The key correctness property guarded here is that a **v5** Orchard bundle at NU6.3 routes
/// to the *fixed* (post-NU6.2) key, NOT the NU6.3 cross-address key — a v5 bundle predates the
/// NU6.3 circuit even when mined at NU6.3.
///
/// This is an async test because forcing the global `Lazy` verifiers builds their `Batch` layer,
/// which spawns a worker task and therefore needs a Tokio runtime.
#[tokio::test(flavor = "multi_thread")]
async fn orchard_verifier_routing_selects_the_correct_key() {
    let pre: &'static super::VerifierService = &VERIFIER_PRE_NU6_2;
    let post: &'static super::VerifierService = &VERIFIER_POST_NU6_2;
    let post_nu6_3: &'static super::VerifierService = &VERIFIER_POST_NU6_3;

    // v5 Orchard bundles before NU6.2 (incl. upgrades from before Orchard existed) route to the
    // insecure key, the only key pre-NU6.2 Orchard history verifies under.
    for nu in [
        NetworkUpgrade::Nu5,
        NetworkUpgrade::Nu6,
        NetworkUpgrade::Nu6_1,
    ] {
        assert!(
            std::ptr::eq(orchard_v5_verifier_for(nu), pre),
            "v5 Orchard at {nu:?} must route to the pre-NU6.2 (insecure) verifier"
        );
    }

    // v5 Orchard bundles from NU6.2 onward route to the fixed key — including at NU6.3 and NU7,
    // because a v5 bundle never uses the NU6.3 cross-address circuit. This is the regression guard
    // for the routing bug.
    for nu in [
        NetworkUpgrade::Nu6_2,
        NetworkUpgrade::Nu6_3,
        NetworkUpgrade::Nu7,
    ] {
        assert!(
            std::ptr::eq(orchard_v5_verifier_for(nu), post),
            "v5 Orchard at {nu:?} must route to the post-NU6.2 (fixed) verifier, not the NU6.3 key"
        );
    }

    // v6 Orchard + Ironwood bundles route to the NU6.3 (cross-address) key.
    assert!(
        std::ptr::eq(orchard_v6_verifier(), post_nu6_3),
        "v6 Orchard/Ironwood must route to the post-NU6.3 (Ironwood) verifier"
    );
}
