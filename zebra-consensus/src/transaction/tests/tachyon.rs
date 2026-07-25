//! Tests for V7 (tachyon) transaction verification.

use std::{collections::HashMap, sync::Arc};

use chrono::Utc;
use tower::ServiceExt;

use zebra_chain::{
    block::Height,
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    transaction::{HashType, LockTime, Transaction},
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use zcash_tachyon::{
    action, bundle, effect,
    entropy::ActionEntropy,
    keys::private,
    note::{CommitmentTrapdoor, Note, NullifierTrapdoor},
    value, PointerStamp, ProofStamp, Tachygram, TachyonBundle,
};

use crate::{
    error::TransactionError,
    transaction::{Request, Response, Verifier},
};

/// A regtest network with NU7 scheduled, and NU7's activation height.
fn nu7_network() -> (Network, Height) {
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(6),
            nu7: Some(10),
            ..Default::default()
        }
        .into(),
    );
    let height = NetworkUpgrade::Nu7
        .activation_height(&network)
        .expect("NU7 activation height is configured");
    (network, height)
}

/// A V7 transaction with the given tachyon bundle and no other transfers.
fn v7_transaction(
    network_upgrade: NetworkUpgrade,
    tachyon_bundle: Option<TachyonBundle>,
) -> Transaction {
    Transaction::V7 {
        network_upgrade,
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: Height(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        ironwood_shielded_data: None,
        tachyon_shielded_data: tachyon_bundle.map(Into::into),
    }
}

/// A proof stamp with an unasserted covered-actions digest and no proof: tx-level verification
/// never checks the stamp's proof or coverage (those are block-level rules), only its tachygrams.
fn mock_proof_stamp(tachygrams: Vec<Tachygram>) -> ProofStamp {
    ProofStamp {
        actions: [0u8; 32],
        tachygrams,
        anchor: zcash_tachyon::Anchor::read(&[0u8; 64][..]).expect("zero anchor reads"),
        proof: Box::new(ragu::Proof::trivial()),
    }
}

/// A random note worth `value` zatoshis, with its spending key.
fn random_note(
    rng: &mut (impl rand::RngCore + rand::CryptoRng),
    value: u64,
) -> (private::SpendingKey, Note) {
    let sk = private::SpendingKey::random(rng);
    let note = Note {
        pk: sk.derive_payment_key(),
        value: value::Positive::try_from(value).expect("value is positive and below MAX_MONEY"),
        psi: NullifierTrapdoor::random(rng),
        rcm: CommitmentTrapdoor::random(rng),
    };
    (sk, note)
}

/// A properly-signed spend-only bundle over `sighash`, in the unproven state.
///
/// A spend contributes its value positively to the transaction value pool, so the resulting
/// transaction has a valid (non-negative) miner fee.
fn signed_spend_bundle(
    sighash: &[u8; 32],
    value: u64,
) -> zcash_tachyon::Bundle<zcash_tachyon::Unproven> {
    let mut rng = rand::thread_rng();

    let (sk, note) = random_note(&mut rng, value);
    let ask = sk.derive_auth_private();

    let theta = ActionEntropy::random(&mut rng);
    let rcv = value::Trapdoor::random(&mut rng);
    let spend_plan = action::Plan::spend(note, theta, rcv, |alpha| {
        ask.derive_action_private(&alpha).derive_action_public()
    });

    let plan = bundle::Plan::new(vec![spend_plan], vec![]);

    plan.sign(&mut rng, sighash, &ask)
        .expect("bundle plan has matching signatures and an in-range value balance")
}

/// The sighash the tachyon bundle's signatures must commit to.
///
/// The current V7 sighash has no tachyon contribution, so it can be computed before the bundle is
/// attached; see the TODO in `verify_v7_transaction`.
fn v7_sighash(tx: &Transaction) -> [u8; 32] {
    tx.sighash(
        NetworkUpgrade::Nu7,
        HashType::ALL,
        Arc::new(Vec::new()),
        None,
    )
    .expect("sighash is computable for a V7 transaction with no transparent inputs")
    .0
}

/// Verifies `tx` through the full transaction verifier with a block request at `height`.
async fn verify_block_transaction(
    network: &Network,
    height: Height,
    tx: Transaction,
) -> Result<Response, TransactionError> {
    // No transparent inputs anywhere in these tests, so the verifier never calls the state.
    let state: MockService<zebra_state::Request, zebra_state::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let verifier = Verifier::new_for_tests(network, state);

    verifier
        .oneshot(Request::Block {
            transaction_hash: tx.hash(),
            transaction: Arc::new(tx),
            known_outpoint_hashes: Arc::new(Default::default()),
            known_utxos: Arc::new(HashMap::new()),
            height,
            time: Utc::now(),
        })
        .await
}

/// A V7 transaction with a properly-signed tachyon bundle passes verification, whether
/// proof-stamped or pointer-stamped: proof coverage is a block-level rule.
#[tokio::test(flavor = "multi_thread")]
async fn v7_with_signed_tachyon_bundle_is_accepted() {
    let _init_guard = zebra_test::init();
    let (network, height) = nu7_network();

    let sighash = v7_sighash(&v7_transaction(NetworkUpgrade::Nu7, None));

    // Proof-stamped: the tx-level rules don't verify the proof itself.
    let proven = signed_spend_bundle(&sighash, 100).stamp(mock_proof_stamp(vec![]));
    let tx = v7_transaction(NetworkUpgrade::Nu7, Some(TachyonBundle::Proven(proven)));
    verify_block_transaction(&network, height, tx)
        .await
        .expect("a signed proof-stamped tachyon transaction should verify");

    // Pointer-stamped: signature checks still run, proof coverage is deferred to the block.
    let adjunct = signed_spend_bundle(&sighash, 100)
        .stamp(mock_proof_stamp(vec![]))
        .strip(PointerStamp::try_from([0xEEu8; 64]).expect("nonzero wtxid"));
    let tx = v7_transaction(NetworkUpgrade::Nu7, Some(TachyonBundle::Adjunct(adjunct)));
    verify_block_transaction(&network, height, tx)
        .await
        .expect("a signed pointer-stamped tachyon transaction should verify");
}

/// A tachyon bundle whose signatures don't commit to the transaction sighash is rejected.
#[tokio::test(flavor = "multi_thread")]
async fn v7_with_wrong_sighash_signatures_is_rejected() {
    let _init_guard = zebra_test::init();
    let (network, height) = nu7_network();

    // Sign over a different message than the transaction's sighash.
    let bundle = signed_spend_bundle(&[0x42u8; 32], 100).stamp(mock_proof_stamp(vec![]));
    let tx = v7_transaction(NetworkUpgrade::Nu7, Some(TachyonBundle::Proven(bundle)));

    let result = verify_block_transaction(&network, height, tx).await;
    assert!(
        matches!(result, Err(TransactionError::TachyonSignatureInvalid(_))),
        "expected TachyonSignatureInvalid, got: {result:?}",
    );
}

/// A tachyon action whose value commitment is the identity point is rejected.
#[tokio::test(flavor = "multi_thread")]
async fn v7_with_identity_cv_is_rejected() {
    use halo2::pasta::group::prime::PrimeCurveAffine;

    let _init_guard = zebra_test::init();
    let (network, height) = nu7_network();

    let mut rng = rand::thread_rng();

    // A structurally valid action, except its cv is the identity point. The identity check runs
    // before signature verification, so the dummy signature bytes are never checked.
    let (_, note) = random_note(&mut rng, 100);
    let theta = ActionEntropy::random(&mut rng);
    let alpha = theta.randomizer::<effect::Output>(note.commitment());
    let rk = private::ActionSigningKey::new(&alpha).derive_action_public();

    let action = zcash_tachyon::Action {
        cv: value::Commitment::from(halo2::pasta::pallas::Affine::identity()),
        rk,
        sig: action::Signature::read(&[0x01u8; 64][..]).expect("64 bytes"),
    };

    let bundle = zcash_tachyon::Bundle {
        actions: vec![action],
        value_balance: value::Balance::ZERO,
        binding_sig: bundle::Signature::read(&[0x02u8; 64][..]).expect("64 bytes"),
        stamp: mock_proof_stamp(vec![]),
    };
    let tx = v7_transaction(NetworkUpgrade::Nu7, Some(TachyonBundle::Proven(bundle)));

    let result = verify_block_transaction(&network, height, tx).await;
    assert!(
        matches!(result, Err(TransactionError::TachyonIdentityAction(_))),
        "expected TachyonIdentityAction, got: {result:?}",
    );
}

/// A proof stamp with duplicate tachygrams is rejected.
#[tokio::test(flavor = "multi_thread")]
async fn v7_with_duplicate_tachygrams_is_rejected() {
    let _init_guard = zebra_test::init();
    let (network, height) = nu7_network();

    let sighash = v7_sighash(&v7_transaction(NetworkUpgrade::Nu7, None));

    // A duplicated tachygram list is sorted (parsing allows it), but not distinct.
    let tachygram = Tachygram::from(halo2::pasta::pallas::Base::from(42u64));
    let bundle =
        signed_spend_bundle(&sighash, 100).stamp(mock_proof_stamp(vec![tachygram, tachygram]));
    let tx = v7_transaction(NetworkUpgrade::Nu7, Some(TachyonBundle::Proven(bundle)));

    let result = verify_block_transaction(&network, height, tx).await;
    assert!(
        matches!(result, Err(TransactionError::TachyonDuplicateTachygram)),
        "expected TachyonDuplicateTachygram, got: {result:?}",
    );
}

/// V7 transactions are rejected before NU7 activation.
#[tokio::test(flavor = "multi_thread")]
async fn v7_is_rejected_before_nu7() {
    let _init_guard = zebra_test::init();
    let (network, _) = nu7_network();

    let nu6_3_height = NetworkUpgrade::Nu6_3
        .activation_height(&network)
        .expect("NU6.3 activation height is configured");

    // The bundle's actions let the transaction pass the has-inputs-and-outputs check, so the
    // network-upgrade gate is what rejects it. The gate errors before any signature check runs,
    // so the signatures can be arbitrary.
    let bundle = signed_spend_bundle(&[0u8; 32], 100).stamp(mock_proof_stamp(vec![]));
    let tx = v7_transaction(NetworkUpgrade::Nu6_3, Some(TachyonBundle::Proven(bundle)));

    let result = verify_block_transaction(&network, nu6_3_height, tx).await;
    assert!(
        matches!(
            result,
            Err(TransactionError::UnsupportedByNetworkUpgrade(7, _))
        ),
        "expected UnsupportedByNetworkUpgrade, got: {result:?}",
    );
}
