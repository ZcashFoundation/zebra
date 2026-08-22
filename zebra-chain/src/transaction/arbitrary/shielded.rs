//! Builders for shielded bundles on the `zcash_primitives` transaction types.
//!
//! Zebra's [`Transaction`](crate::transaction::Transaction) wraps
//! `zcash_primitives::transaction::Transaction`, whose bundles are owned by the upstream crates
//! and cannot be mutated in place. Tests therefore cannot reach in and edit shielded data the way
//! they could when `Transaction` was an enum of Zebra-owned structs.
//!
//! This module builds Orchard-shaped bundles (used by both the Orchard and Ironwood pools)
//! directly from their constituent parts, so property tests and vector tests can exercise
//! shielded code paths.
//!
//! The bundles are structurally valid — they carry real, canonically-encoded Pallas points and a
//! proof of the canonical length, so they serialize, deserialize, and commit correctly — but they
//! are **not** consensus-valid: the proofs are zeroed and the signatures are arbitrary. They are
//! for exercising parsing, indexing, and nullifier bookkeeping, not proof verification.

use group::{
    ff::{FromUniformBytes, PrimeField},
    prime::PrimeCurveAffine,
    GroupEncoding,
};
use halo2::pasta::pallas;
use nonempty::NonEmpty;

use orchard::{
    bundle::{Authorized, BundleVersion, Flags},
    note::{ExtractedNoteCommitment, Nullifier, TransmittedNoteCiphertext},
    primitives::redpallas::{self, SpendAuth},
    value::ValueCommitment,
    Action, Anchor, Bundle, Proof,
};
use zcash_protocol::value::ZatBalance;

/// Derives a canonically-encoded `pallas::Base` from a `seed`.
///
/// Distinct seeds give distinct field elements, so callers can build actions with distinct
/// nullifiers and note commitments.
fn base_from_seed(seed: u64) -> pallas::Base {
    let mut bytes = [0u8; 64];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    // Domain-separate from `scalar_from_seed` so the two never coincide.
    bytes[63] = 0x01;
    pallas::Base::from_uniform_bytes(&bytes)
}

/// Derives a canonically-encoded `pallas::Scalar` from a `seed`.
fn scalar_from_seed(seed: u64) -> pallas::Scalar {
    let mut bytes = [0u8; 64];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    bytes[63] = 0x02;
    pallas::Scalar::from_uniform_bytes(&bytes)
}

/// Builds a non-identity RedPallas verification key from a `seed`.
///
/// [`Action::from_parts`] rejects an identity `rk`, so this derives the key from a signing key
/// rather than picking bytes directly.
fn verification_key_from_seed(seed: u64) -> redpallas::VerificationKey<SpendAuth> {
    let sk_bytes = scalar_from_seed(seed).to_repr();
    let sk = reddsa::SigningKey::<reddsa::orchard::SpendAuth>::try_from(sk_bytes)
        .expect("a canonical scalar is a valid signing key");
    let pk_bytes: [u8; 32] = reddsa::VerificationKey::from(&sk).into();

    redpallas::VerificationKey::try_from(pk_bytes)
        .expect("a key derived from a signing key is a valid, non-identity verification key")
}

/// Builds a structurally valid Orchard [`Action`] from a `seed`.
///
/// Each `seed` yields a distinct nullifier and note commitment, so a bundle's actions do not
/// collide, and bundles built from different seed ranges have disjoint nullifier sets.
fn fake_action(seed: u64) -> Action<redpallas::Signature<SpendAuth>> {
    // `cv_net` is unconstrained by `Action::from_parts`, so the identity point is fine here;
    // it just has to be a canonical point encoding.
    let cv_net = ValueCommitment::from_bytes(&pallas::Affine::identity().to_bytes())
        .expect("the identity point is a canonical value commitment encoding");

    let nullifier = Nullifier::from_bytes(&base_from_seed(seed).to_repr())
        .expect("a canonical base field element is a valid nullifier");

    let cmx = ExtractedNoteCommitment::from_bytes(&base_from_seed(seed ^ 0xFFFF).to_repr())
        .expect("a canonical base field element is a valid note commitment");

    // `Action::from_parts` rejects an identity ephemeral key, so use the curve generator.
    let encrypted_note = TransmittedNoteCiphertext {
        epk_bytes: pallas::Affine::generator().to_bytes(),
        enc_ciphertext: [0u8; 580],
        out_ciphertext: [0u8; 80],
    };

    let spend_auth_sig = redpallas::Signature::<SpendAuth>::from([0u8; 64]);

    Action::from_parts(
        nullifier,
        verification_key_from_seed(seed),
        cmx,
        encrypted_note,
        cv_net,
        spend_auth_sig,
    )
    .expect("action parts are valid: rk and epk are non-identity")
}

/// Builds a structurally valid Orchard-shaped bundle.
///
/// * `flags` must be representable under `bundle_version`, otherwise this panics. In particular
///   the `enableCrossAddress` bit is representable only for [`BundleVersion::ironwood_v3`].
/// * `n_actions` must be non-zero.
/// * `seed` selects the action contents; bundles built with different seeds have disjoint
///   nullifier sets, which matters for the mempool and state conflict tests.
///
/// The proof is zeroed but has the canonical length for `n_actions`, so the bundle passes
/// [`Bundle::try_from_parts`]'s proof-size check and Zebra's own canonical-size rule.
pub fn fake_orchard_bundle(
    flags: Flags,
    value_balance: ZatBalance,
    n_actions: usize,
    seed: u64,
    bundle_version: BundleVersion,
) -> Bundle<Authorized, ZatBalance> {
    assert!(n_actions > 0, "an Orchard bundle must have some actions");

    let actions: Vec<_> = (0..n_actions)
        .map(|i| fake_action(seed.wrapping_add(i as u64)))
        .collect();

    let authorization = Authorized::from_parts(
        Proof::new(vec![0u8; Proof::expected_proof_size(n_actions)]),
        redpallas::Signature::<redpallas::Binding>::from([0u8; 64]),
    );

    Bundle::try_from_parts(
        NonEmpty::from_vec(actions).expect("n_actions is non-zero"),
        flags,
        value_balance,
        Anchor::from(base_from_seed(seed ^ 0xA11C)),
        authorization,
        bundle_version,
    )
    .expect("the proof has the canonical length and the flags are representable")
}

/// Builds an Orchard-shaped bundle whose actions all share a single nullifier.
///
/// Used by duplicate-nullifier consensus tests, which need a within-transaction double spend.
pub fn fake_orchard_bundle_duplicate_nullifiers(
    flags: Flags,
    value_balance: ZatBalance,
    n_actions: usize,
    seed: u64,
    bundle_version: BundleVersion,
) -> Bundle<Authorized, ZatBalance> {
    assert!(n_actions > 0, "an Orchard bundle must have some actions");

    // Every action is built from the same seed, so they share a nullifier.
    let actions: Vec<_> = (0..n_actions).map(|_| fake_action(seed)).collect();

    let authorization = Authorized::from_parts(
        Proof::new(vec![0u8; Proof::expected_proof_size(n_actions)]),
        redpallas::Signature::<redpallas::Binding>::from([0u8; 64]),
    );

    Bundle::try_from_parts(
        NonEmpty::from_vec(actions).expect("n_actions is non-zero"),
        flags,
        value_balance,
        Anchor::from(base_from_seed(seed ^ 0xA11C)),
        authorization,
        bundle_version,
    )
    .expect("the proof has the canonical length and the flags are representable")
}

/// The seed used for fields the note-encryption vectors do not constrain.
const NOTE_VECTOR_SEED: u64 = 0x4E4F_5445;

/// Builds a single-action Orchard-shaped bundle carrying caller-supplied note-encryption fields.
///
/// The note-encryption test vectors fix `cv_net`, `rho`, `cmx`, `ephemeralKey`, `encCiphertext`
/// and `outCiphertext`; everything else (the spend authorization key and signature, the anchor,
/// the proof) is unconstrained by those vectors and is filled in with the same dummy values as
/// [`fake_orchard_bundle`].
///
/// `flags` must have outputs enabled for the resulting transaction to count as having shielded
/// outputs, and must be representable under `bundle_version`.
#[allow(clippy::too_many_arguments)]
pub fn fake_orchard_bundle_with_note(
    flags: Flags,
    value_balance: ZatBalance,
    bundle_version: BundleVersion,
    cv_net: &[u8; 32],
    nullifier: &[u8; 32],
    cmx: &[u8; 32],
    epk_bytes: [u8; 32],
    enc_ciphertext: [u8; 580],
    out_ciphertext: [u8; 80],
) -> Bundle<Authorized, ZatBalance> {
    let action = Action::from_parts(
        Nullifier::from_bytes(nullifier).expect("the test vector's rho is a valid nullifier"),
        // `rk` is not covered by the note-encryption vectors, and decryption does not read it.
        verification_key_from_seed(NOTE_VECTOR_SEED),
        ExtractedNoteCommitment::from_bytes(cmx)
            .expect("the test vector's cmx is a valid note commitment"),
        TransmittedNoteCiphertext {
            epk_bytes,
            enc_ciphertext,
            out_ciphertext,
        },
        ValueCommitment::from_bytes(cv_net).expect("the test vector's cv_net is a valid point"),
        redpallas::Signature::<SpendAuth>::from([0u8; 64]),
    )
    .expect("the test vector supplies a non-identity rk and ephemeral key");

    let authorization = Authorized::from_parts(
        Proof::new(vec![0u8; Proof::expected_proof_size(1)]),
        redpallas::Signature::<redpallas::Binding>::from([0u8; 64]),
    );

    Bundle::try_from_parts(
        NonEmpty::from_vec(vec![action]).expect("exactly one action"),
        flags,
        value_balance,
        Anchor::from(base_from_seed(0xA11C)),
        authorization,
        bundle_version,
    )
    .expect("the proof has the canonical length and the flags are representable")
}

/// Returns the flag set with outputs enabled that is representable under `bundle_version`.
///
/// The Orchard pool requires cross-address transfers to be enabled before NU6.3 and disabled
/// from NU6.3 onward, so the representable flag set depends on the version.
pub fn outputs_enabled_flags(bundle_version: BundleVersion) -> Flags {
    if Flags::ENABLED.to_byte(bundle_version).is_some() {
        Flags::ENABLED
    } else {
        Flags::CROSS_ADDRESS_DISABLED
    }
}

/// Builds a bundle for `pool` that is valid under `branch_id`, or `None` if the pool is not
/// defined for that consensus branch (Ironwood before NU6.3).
///
/// The bundle version — and therefore which flag sets are representable — is derived from
/// `branch_id`, so a bundle built here always matches the transaction it will be placed in. The
/// flags are the maximal set representable under that version: the Orchard pool disables
/// cross-address transfers from NU6.3 onward and requires them enabled before, while the
/// Ironwood pool encodes the choice in its flag byte.
pub fn fake_bundle_for_branch(
    branch_id: zcash_protocol::consensus::BranchId,
    pool: ::orchard::ValuePool,
    n_actions: usize,
    seed: u64,
) -> Option<Bundle<Authorized, ZatBalance>> {
    let bundle_version =
        zcash_primitives::transaction::components::orchard::bundle_version_for_branch(
            branch_id, pool,
        )?;

    // Pick the flag set that this bundle version can encode.
    let flags = if Flags::ENABLED.to_byte(bundle_version).is_some() {
        Flags::ENABLED
    } else {
        Flags::CROSS_ADDRESS_DISABLED
    };

    Some(fake_orchard_bundle(
        flags,
        ZatBalance::from_i64(0).expect("zero is a valid balance"),
        n_actions,
        seed,
        bundle_version,
    ))
}

/// The size of one serialized Orchard action: cv, nullifier, rk, cmx and ephemeralKey
/// (32 bytes each), then encCiphertext and outCiphertext.
pub const ACTION_WIRE_SIZE: usize = 32 * 5 + 580 + 80;

/// The offset of the `flagsOrchard` byte of the Orchard bundle, within a v6 transaction that has
/// no transparent and no Sapling bundle, and whose Orchard bundle has `n_actions` actions.
///
/// Layout: header, nVersionGroupId, nConsensusBranchId, lockTime, nExpiryHeight (4 bytes each),
/// then the empty transparent bundle (two zero CompactSize counts), the empty Sapling bundle
/// (two zero CompactSize counts), then `nActionsOrchard` and the actions themselves.
///
/// `n_actions` must be small enough to encode as a one-byte CompactSize.
pub fn v6_orchard_flags_offset(n_actions: usize) -> usize {
    assert!(n_actions < 253, "n_actions must be a one-byte CompactSize");
    (4 * 5) + 2 + 2 + 1 + n_actions * ACTION_WIRE_SIZE
}

/// The offset of the `flagsOrchard` byte of the *Ironwood* bundle, within a v6 transaction that
/// has no transparent and no Sapling bundle, an *empty* Orchard bundle, and an Ironwood bundle
/// with `n_ironwood_actions` actions.
///
/// An empty Orchard bundle is a single zero `nActionsOrchard` byte.
pub fn v6_ironwood_flags_offset(n_ironwood_actions: usize) -> usize {
    assert!(
        n_ironwood_actions < 253,
        "n_ironwood_actions must be a one-byte CompactSize"
    );
    (4 * 5) + 2 + 2 + 1 + 1 + n_ironwood_actions * ACTION_WIRE_SIZE
}

/// The offset of the first Orchard action's `rk` field, within a v5 transaction that has no
/// transparent and no Sapling bundle.
///
/// Layout: header, nVersionGroupId, nConsensusBranchId, lockTime, nExpiryHeight (4 bytes each),
/// then the empty transparent bundle (two zero CompactSize counts), the empty Sapling bundle
/// (two zero CompactSize counts), `nActionsOrchard`, then the action's `cv` and `nullifier`.
pub const V5_FIRST_ACTION_RK_OFFSET: usize = (4 * 5) + 2 + 2 + 1 + 32 + 32;

/// Builds a V6 transaction carrying the given Orchard and Ironwood bundles.
///
/// This is the constructor the v6 wire-format vector tests use: it takes bundles built by
/// [`fake_orchard_bundle`] and puts them in the two v6 Orchard-shaped slots.
pub fn fake_v6_transaction(
    network_upgrade: crate::parameters::NetworkUpgrade,
    orchard_bundle: Option<Bundle<Authorized, ZatBalance>>,
    ironwood_bundle: Option<Bundle<Authorized, ZatBalance>>,
) -> crate::transaction::Transaction {
    crate::transaction::Transaction::test_v6_with_bundles(
        network_upgrade,
        Vec::new(),
        Vec::new(),
        crate::transaction::LockTime::min_lock_time_timestamp(),
        crate::block::Height(0),
        orchard_bundle,
        ironwood_bundle,
    )
}

/// Returns a copy of `tx` whose Orchard bundle carries `value_balance`.
///
/// The bundle is owned by `zcash_primitives` and cannot be mutated in place, so this rebuilds
/// the transaction. Panics if `tx` has no Orchard bundle.
pub fn with_orchard_value_balance(
    tx: crate::transaction::Transaction,
    value_balance: i64,
) -> crate::transaction::Transaction {
    let balance = ZatBalance::from_i64(value_balance).expect("a valid signed amount");

    let bundle = tx
        .orchard_bundle()
        .expect("the transaction must have an Orchard bundle")
        .clone()
        .try_map_value_balance::<_, (), _>(|_| Ok(balance))
        .expect("the mapping cannot fail");

    tx.with_orchard_bundle(Some(bundle))
}

/// Returns a copy of `tx` carrying a dummy single-action Orchard bundle.
///
/// The bundle has no flags set and a zero value balance, for structural consensus-rule tests
/// that only care that *an* Orchard bundle is present.
pub fn insert_fake_orchard_shielded_data(
    tx: crate::transaction::Transaction,
) -> crate::transaction::Transaction {
    let branch_id = tx.inner().consensus_branch_id();
    let bundle = fake_bundle_for_branch(branch_id, ::orchard::ValuePool::Orchard, 1, 0xF00D)
        .expect("the Orchard pool is defined for this transaction's branch");

    tx.with_orchard_bundle(Some(bundle))
}

/// Returns a copy of `tx` whose Orchard bundle carries `flags`.
///
/// Panics if `tx` has no Orchard bundle, or if `flags` are not representable under the bundle's
/// version.
pub fn with_orchard_flags(
    tx: crate::transaction::Transaction,
    flags: Flags,
) -> crate::transaction::Transaction {
    let bundle = tx
        .orchard_bundle()
        .expect("the transaction must have an Orchard bundle");

    let rebuilt = Bundle::try_from_parts(
        bundle.actions().clone(),
        flags,
        *bundle.value_balance(),
        *bundle.anchor(),
        bundle.authorization().clone(),
        bundle.bundle_version(),
    )
    .expect("the flags must be representable under the bundle version");

    tx.with_orchard_bundle(Some(rebuilt))
}

/// Returns a copy of `tx` whose Orchard bundle keeps its effects but has garbage authorizing
/// data: a corrupt proof, binding signature, and spend authorization signatures.
///
/// Per ZIP-244 the txid covers only a transaction's effects, so the returned transaction has the
/// same txid as `tx` while being unverifiable. Used to check that a cached verification result
/// for one cannot stand in for the other.
///
/// The bundle version must not enforce a canonical proof size (i.e. pre-NU6.2 Orchard), since the
/// point is to install a proof that is not a real one.
pub fn with_garbage_orchard_authorization(
    tx: crate::transaction::Transaction,
) -> crate::transaction::Transaction {
    let bundle = tx
        .orchard_bundle()
        .expect("the transaction must have an Orchard bundle");

    // Rebuild every action with a garbage spend authorization signature, preserving its effects.
    let actions: Vec<_> = bundle
        .actions()
        .iter()
        .map(|action| {
            Action::from_parts(
                *action.nullifier(),
                action.rk().clone(),
                *action.cmx(),
                TransmittedNoteCiphertext {
                    epk_bytes: action.encrypted_note().epk_bytes,
                    enc_ciphertext: action.encrypted_note().enc_ciphertext,
                    out_ciphertext: action.encrypted_note().out_ciphertext,
                },
                action.cv_net().clone(),
                redpallas::Signature::<SpendAuth>::from([0xFF; 64]),
            )
            .expect("the effects are copied from a valid action")
        })
        .collect();

    let authorization = Authorized::from_parts(
        Proof::new(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        redpallas::Signature::<redpallas::Binding>::from([0xFF; 64]),
    );

    let rebuilt = Bundle::try_from_parts(
        NonEmpty::from_vec(actions).expect("the source bundle is non-empty"),
        *bundle.flags(),
        *bundle.value_balance(),
        *bundle.anchor(),
        authorization,
        bundle.bundle_version(),
    )
    .expect("the bundle version must not enforce a canonical proof size");

    tx.with_orchard_bundle(Some(rebuilt))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        parameters::NetworkUpgrade,
        serialization::{ZcashDeserializeInto, ZcashSerialize},
        transaction::Transaction,
    };

    /// The bundles this module builds must survive a wire-format round trip, otherwise the tests
    /// that rely on them are testing a shape the parser would never accept.
    #[test]
    fn fake_bundles_round_trip() {
        let zero = ZatBalance::from_i64(0).expect("zero is a valid balance");

        let orchard = fake_orchard_bundle(
            Flags::CROSS_ADDRESS_DISABLED,
            zero,
            2,
            1,
            BundleVersion::orchard_v3(),
        );
        let ironwood =
            fake_orchard_bundle(Flags::ENABLED, zero, 1, 1000, BundleVersion::ironwood_v3());

        let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, Some(orchard), Some(ironwood));

        assert_eq!(tx.orchard_actions().count(), 2);
        assert_eq!(tx.ironwood_actions().count(), 1);

        let bytes = tx
            .zcash_serialize_to_vec()
            .expect("a v6 transaction with fake bundles serializes");
        let tx2: Transaction = bytes
            .zcash_deserialize_into()
            .expect("a v6 transaction with fake bundles deserializes");

        assert_eq!(tx.hash(), tx2.hash());
        assert_eq!(tx2.orchard_actions().count(), 2);
        assert_eq!(tx2.ironwood_actions().count(), 1);
    }

    /// Bundles built from different seeds must have disjoint nullifier sets, which the mempool
    /// and state conflict tests rely on.
    #[test]
    fn fake_bundles_have_distinct_nullifiers() {
        let zero = ZatBalance::from_i64(0).expect("zero is a valid balance");

        // `orchard_v3` requires cross-address transfers to be disabled.
        let a = fake_orchard_bundle(
            Flags::CROSS_ADDRESS_DISABLED,
            zero,
            3,
            0,
            BundleVersion::orchard_v3(),
        );
        let b = fake_orchard_bundle(
            Flags::CROSS_ADDRESS_DISABLED,
            zero,
            3,
            100,
            BundleVersion::orchard_v3(),
        );

        let nfs = |bundle: &Bundle<Authorized, ZatBalance>| {
            bundle
                .actions()
                .iter()
                .map(|action| action.nullifier().to_bytes())
                .collect::<Vec<_>>()
        };

        let (a_nfs, b_nfs) = (nfs(&a), nfs(&b));

        // Within a bundle.
        let mut sorted = a_nfs.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            3,
            "actions in a bundle have distinct nullifiers"
        );

        // Across bundles.
        for nf in &a_nfs {
            assert!(
                !b_nfs.contains(nf),
                "bundles from different seeds must not collide"
            );
        }
    }
}
