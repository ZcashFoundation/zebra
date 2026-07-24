//! Tests for the block-level tachyon consensus rules.

use std::sync::Arc;

use zebra_chain::{
    block::{Block, Height},
    parameters::NetworkUpgrade,
    serialization::ZcashDeserialize,
    transaction::{LockTime, Transaction, WtxId},
};

use zcash_tachyon::{
    action, bundle, effect,
    entropy::ActionEntropy,
    keys::private,
    note::{CommitmentTrapdoor, Note, NullifierTrapdoor},
    value, Anchor, PointerStamp, ProofStamp, Tachygram, TachyonBundle,
};

use crate::error::BlockError;

use super::coherence;

/// Computes `hStampActionsTachyon` over already-sorted descriptor bytes.
///
/// This duplicates tachyon's private `blake2b::action_descriptor_digest` (BLAKE2b-256,
/// personalization `Tachyon-Actions`, over the concatenated 64-byte descriptors). Every test
/// using it self-validates via [`ProofStamp::covers`], so drift from the tachyon crate fails
/// loudly instead of silently testing against the wrong digest.
fn action_descriptor_digest(descriptors: &[action::Descriptor]) -> [u8; 32] {
    // `Vec<[u8; 64]>: FromIterator<Descriptor>` is tachyon's public descriptor-bytes conversion.
    let mut descriptor_bytes: Vec<[u8; 64]> = descriptors.iter().copied().collect();
    descriptor_bytes.sort_unstable();

    let mut state = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"Tachyon-Actions")
        .to_state();
    for descriptor in &descriptor_bytes {
        state.update(descriptor);
    }
    state
        .finalize()
        .as_bytes()
        .try_into()
        .expect("hash length is 32")
}

/// A structurally valid action with a random `rk` and dummy signature: block-level coherence
/// never checks signatures.
fn dummy_action() -> zcash_tachyon::Action {
    use halo2::pasta::group::prime::PrimeCurveAffine;

    let mut rng = rand::thread_rng();

    let sk = private::SpendingKey::random(&mut rng);
    let note = Note {
        pk: sk.derive_payment_key(),
        value: value::Positive::try_from(100u64).expect("valid value"),
        psi: NullifierTrapdoor::random(&mut rng),
        rcm: CommitmentTrapdoor::random(&mut rng),
    };
    let alpha = ActionEntropy::random(&mut rng).randomizer::<effect::Output>(note.commitment());
    let rk = private::ActionSigningKey::new(&alpha).derive_action_public();

    zcash_tachyon::Action {
        cv: value::Commitment::from(halo2::pasta::pallas::Affine::generator()),
        rk,
        sig: action::Signature::read(&[0x01u8; 64][..]).expect("64 bytes"),
    }
}

/// The all-zero anchor (height 0, identity pool commit).
fn zero_anchor() -> Anchor {
    Anchor::read(&[0u8; 64][..]).expect("zero anchor reads")
}

/// A proof-stamped bundle with the given actions, tachygrams, and covered-actions digest.
fn proven_bundle(
    mut actions: Vec<zcash_tachyon::Action>,
    mut tachygrams: Vec<Tachygram>,
    covered_actions: [u8; 32],
) -> TachyonBundle {
    actions.sort_unstable();
    tachygrams.sort_unstable();
    TachyonBundle::Proven(zcash_tachyon::Bundle {
        actions,
        value_balance: value::Balance::ZERO,
        binding_sig: bundle::Signature::read(&[0x02u8; 64][..]).expect("64 bytes"),
        stamp: ProofStamp {
            actions: covered_actions,
            tachygrams,
            anchor: zero_anchor(),
            proof: Box::new(ragu::Proof::trivial()),
        },
    })
}

/// A pointer-stamped bundle with the given actions, naming `target` as its covering aggregate.
fn adjunct_bundle(mut actions: Vec<zcash_tachyon::Action>, target: [u8; 64]) -> TachyonBundle {
    actions.sort_unstable();
    TachyonBundle::Adjunct(zcash_tachyon::Bundle {
        actions,
        value_balance: value::Balance::ZERO,
        binding_sig: bundle::Signature::read(&[0x02u8; 64][..]).expect("64 bytes"),
        stamp: PointerStamp::try_from(target).expect("nonzero wtxid"),
    })
}

/// A V7 transaction carrying only the given tachyon bundle.
fn v7_transaction(tachyon_bundle: TachyonBundle) -> Arc<Transaction> {
    Arc::new(Transaction::V7 {
        network_upgrade: NetworkUpgrade::Nu7,
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: Height(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        ironwood_shielded_data: None,
        tachyon_shielded_data: Some(tachyon_bundle.into()),
    })
}

/// A block containing the given transactions (the header contents are irrelevant to coherence).
fn block_with(transactions: Vec<Arc<Transaction>>) -> Block {
    let template = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
        .expect("hardcoded test block deserializes");

    Block {
        header: template.header,
        transactions,
    }
}

fn tachygram(value: u64) -> Tachygram {
    Tachygram::from(halo2::pasta::pallas::Base::from(value))
}

/// Rule 1: two proof stamps sharing a tachygram anywhere in the block are rejected.
#[test]
fn duplicate_tachygram_across_block_is_rejected() {
    let _init_guard = zebra_test::init();

    let action_a = dummy_action();
    let action_b = dummy_action();
    let digest_a = action_descriptor_digest(&[action_a.descriptor()]);
    let digest_b = action_descriptor_digest(&[action_b.descriptor()]);

    let block = block_with(vec![
        v7_transaction(proven_bundle(
            vec![action_a],
            vec![tachygram(1), tachygram(2)],
            digest_a,
        )),
        v7_transaction(proven_bundle(
            vec![action_b],
            vec![tachygram(2), tachygram(3)],
            digest_b,
        )),
    ]);

    assert_eq!(
        coherence(&block).err(),
        Some(BlockError::DuplicateTachygram)
    );
}

/// Rule 2: a pointer stamp naming a wtxid with no proof-stamped transaction in the block is
/// rejected.
#[test]
fn pointer_to_missing_aggregate_is_rejected() {
    let _init_guard = zebra_test::init();

    let block = block_with(vec![v7_transaction(adjunct_bundle(
        vec![dummy_action()],
        [0xEEu8; 64],
    ))]);

    assert_eq!(
        coherence(&block).err(),
        Some(BlockError::TachyonAggregateNotFound)
    );
}

/// Rule 3: a proof stamp whose carried covered-actions digest doesn't match its actions is
/// rejected.
#[test]
fn coverage_mismatch_is_rejected() {
    let _init_guard = zebra_test::init();

    let block = block_with(vec![v7_transaction(proven_bundle(
        vec![dummy_action()],
        vec![],
        // Deliberately not the digest of the bundle's actions.
        [0u8; 32],
    ))]);

    assert_eq!(
        coherence(&block).err(),
        Some(BlockError::TachyonCoverageMismatch)
    );
}

/// Rules 2-3 positive path: an aggregate covering its own actions plus a pointing adjunct's
/// actions passes coherence, and the returned work item carries the combined descriptor set.
#[test]
fn aggregate_with_adjunct_passes_coherence() {
    let _init_guard = zebra_test::init();

    let own_action = dummy_action();
    let adjunct_action = dummy_action();

    let covered_digest =
        action_descriptor_digest(&[own_action.descriptor(), adjunct_action.descriptor()]);
    let aggregate_tx = v7_transaction(proven_bundle(
        vec![own_action],
        vec![tachygram(1)],
        covered_digest,
    ));

    // Self-validation: the hand-rolled digest must agree with tachyon's `covers`.
    {
        let Some(data) = aggregate_tx.tachyon_shielded_data() else {
            panic!("aggregate transaction has a tachyon bundle");
        };
        let TachyonBundle::Proven(bundle) = &data.0 else {
            panic!("aggregate transaction bundle is proof-stamped");
        };
        assert!(
            bundle
                .stamp
                .covers(&[own_action.descriptor(), adjunct_action.descriptor()]),
            "hand-rolled action_descriptor_digest drifted from the tachyon crate",
        );
    }

    let aggregate_wtxid = WtxId::from(aggregate_tx.as_ref()).as_bytes();
    let adjunct_tx = v7_transaction(adjunct_bundle(vec![adjunct_action], aggregate_wtxid));

    let block = block_with(vec![aggregate_tx, adjunct_tx]);

    let aggregates = coherence(&block).expect("coherent block passes");
    assert_eq!(aggregates.len(), 1, "one proof stamp to verify");
    assert_eq!(
        aggregates[0].covered_descriptors.len(),
        2,
        "the work item covers the aggregate's own action and the adjunct's action",
    );
}

/// An autonome (proof-stamped bundle covering only its own actions) passes coherence by itself.
#[test]
fn autonome_passes_coherence() {
    let _init_guard = zebra_test::init();

    let action = dummy_action();
    let digest = action_descriptor_digest(&[action.descriptor()]);

    let block = block_with(vec![v7_transaction(proven_bundle(
        vec![action],
        vec![tachygram(7)],
        digest,
    ))]);

    let aggregates = coherence(&block).expect("coherent block passes");
    assert_eq!(aggregates.len(), 1);
    assert_eq!(aggregates[0].covered_descriptors.len(), 1);
}

/// Rule 4 negative path: a proof stamp with a correct covered-actions digest but an invalid
/// (trivial mock) proof fails proof verification.
#[tokio::test(flavor = "multi_thread")]
async fn trivial_proof_fails_verification() {
    let _init_guard = zebra_test::init();

    let action = dummy_action();
    let descriptors = vec![action.descriptor()];
    let stamp = ProofStamp {
        actions: action_descriptor_digest(&descriptors),
        tachygrams: vec![tachygram(1)],
        anchor: zero_anchor(),
        proof: Box::new(ragu::Proof::trivial()),
    };

    let result = crate::primitives::tachyon::verify_proof_stamp(stamp, descriptors).await;
    assert!(
        matches!(result, Err(BlockError::TachyonProofInvalid(_))),
        "expected TachyonProofInvalid, got: {result:?}",
    );
}

/// Rule 4 positive path: a genuinely proven (mock proof system) output stamp verifies.
#[tokio::test(flavor = "multi_thread")]
async fn real_mock_proof_passes_verification() {
    let _init_guard = zebra_test::init();

    let mut rng = rand::thread_rng();

    let sk = private::SpendingKey::random(&mut rng);
    let note = Note {
        pk: sk.derive_payment_key(),
        value: value::Positive::try_from(100u64).expect("valid value"),
        psi: NullifierTrapdoor::random(&mut rng),
        rcm: CommitmentTrapdoor::random(&mut rng),
    };
    let theta = ActionEntropy::random(&mut rng);
    let rcv = value::Trapdoor::random(&mut rng);
    let output_plan = action::Plan::output(note, theta, rcv);
    let descriptors = vec![output_plan.descriptor()];

    let plan = bundle::Plan::new(vec![], vec![output_plan]);
    let pak = sk.derive_proof_private();

    let stamp = plan
        .stamp_plan(zero_anchor())
        .prove(&mut rng, &pak, vec![])
        .expect("output-only stamp plan proves under the mock proof system");

    crate::primitives::tachyon::verify_proof_stamp(stamp, descriptors)
        .await
        .expect("a genuinely proven stamp verifies");
}
