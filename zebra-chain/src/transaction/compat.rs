//! Conversions between Zebra and `zcash_primitives` transaction types.

#[cfg(any(test, feature = "proptest-impl"))]
use zcash_protocol::value::Zatoshis;

use zcash_primitives::transaction::{self as zp_tx};

use crate::{
    amount::Amount,
    block,
    serialization::SerializationError,
    transaction::{self, LockTime},
    transparent::{self, OutPoint, Script},
};

// ── Transparent Input ────────────────────────────────────────────────

/// Convert a librustzcash `TxIn<Authorized>` into a Zebra `transparent::Input`.
///
/// Coinbase inputs are detected by checking for the null `OutPoint`.
pub fn txin_to_input(
    txin: &zcash_transparent::bundle::TxIn<zcash_transparent::bundle::Authorized>,
) -> Result<transparent::Input, SerializationError> {
    if *txin.prevout() == zcash_transparent::bundle::OutPoint::NULL {
        // Coinbase input: the script_sig contains the encoded height + miner data.
        //
        // The genesis coinbase predates BIP-34, so its script has no height prefix and must be
        // special-cased, exactly as `Input::zcash_deserialize` does. Parsing it as a height push
        // would otherwise yield a bogus height instead of `Height::MIN`.
        let script_bytes = txin.script_sig().0 .0.clone();
        let (height, data) = if script_bytes.as_slice()
            == crate::transparent::serialize::GENESIS_COINBASE_SCRIPT_SIG
        {
            (
                block::Height::MIN,
                crate::transparent::serialize::GENESIS_COINBASE_SCRIPT_SIG.to_vec(),
            )
        } else {
            crate::transparent::serialize::parse_coinbase_height(&script_bytes)?
        };
        Ok(transparent::Input::Coinbase {
            height,
            data,
            sequence: txin.sequence(),
        })
    } else {
        let prevout = txin.prevout();
        let hash_bytes: [u8; 32] = *prevout.hash();
        Ok(transparent::Input::PrevOut {
            outpoint: OutPoint {
                hash: transaction::Hash(hash_bytes),
                index: prevout.n(),
            },
            unlock_script: Script::new(&txin.script_sig().0 .0),
            sequence: txin.sequence(),
        })
    }
}

/// Convert a Zebra `transparent::Input` into a librustzcash `TxIn<Authorized>`.
#[cfg(any(test, feature = "proptest-impl"))]
pub fn input_to_txin(
    input: &transparent::Input,
) -> zcash_transparent::bundle::TxIn<zcash_transparent::bundle::Authorized> {
    match input {
        transparent::Input::PrevOut {
            outpoint,
            unlock_script,
            sequence,
        } => {
            let zp_outpoint =
                zcash_transparent::bundle::OutPoint::new(outpoint.hash.0, outpoint.index);
            zcash_transparent::bundle::TxIn::from_parts(
                zp_outpoint,
                zcash_transparent::address::Script(zcash_script::script::Code(
                    unlock_script.as_raw_bytes().to_vec(),
                )),
                *sequence,
            )
        }
        transparent::Input::Coinbase { sequence, .. } => {
            // Reconstruct the full script_sig (encoded height followed by the miner data),
            // matching the wire format. `coinbase_script` handles the genesis special case, and
            // only returns `None` for a hand-built genesis-height input whose data is not the
            // genesis coinbase, which cannot come from a deserialized transaction.
            let script_bytes = input
                .coinbase_script()
                .expect("coinbase_script reconstructs from a deserialized coinbase input");

            zcash_transparent::bundle::TxIn::from_parts(
                zcash_transparent::bundle::OutPoint::NULL,
                zcash_transparent::address::Script(zcash_script::script::Code(script_bytes)),
                *sequence,
            )
        }
    }
}

// ── Transparent Output ───────────────────────────────────────────────

/// Convert a librustzcash `TxOut` into a Zebra `transparent::Output`.
pub fn txout_to_output(txout: &zcash_transparent::bundle::TxOut) -> transparent::Output {
    let value_u64: u64 = txout.value().into();
    transparent::Output {
        value: Amount::try_from(value_u64 as i64)
            .expect("librustzcash Zatoshis is always a valid non-negative Amount"),
        lock_script: Script::new(&txout.script_pubkey().0 .0),
    }
}

/// Convert a Zebra `transparent::Output` into a librustzcash `TxOut`.
#[cfg(any(test, feature = "proptest-impl"))]
pub fn output_to_txout(output: &transparent::Output) -> zcash_transparent::bundle::TxOut {
    let zatoshis = Zatoshis::from_nonnegative_i64(output.value.into())
        .expect("Zebra Amount<NonNegative> is always a valid Zatoshis");
    zcash_transparent::bundle::TxOut::new(
        zatoshis,
        zcash_transparent::address::Script(zcash_script::script::Code(
            output.lock_script.as_raw_bytes().to_vec(),
        )),
    )
}

// ── LockTime ─────────────────────────────────────────────────────────

/// Convert a librustzcash `u32` lock_time into a Zebra `LockTime`.
///
/// Values below 500_000_000 are interpreted as block heights, values at or above
/// as Unix timestamps.
pub fn u32_to_lock_time(lock_time: u32) -> LockTime {
    // Reuse the deserialization logic which implements the same threshold check.
    use crate::serialization::ZcashDeserialize;
    let bytes = lock_time.to_le_bytes();
    LockTime::zcash_deserialize(&bytes[..]).expect("all u32 values are valid LockTimes")
}

/// Convert a Zebra `LockTime` into a `u32` for librustzcash.
#[cfg(any(test, feature = "proptest-impl"))]
pub fn lock_time_to_u32(lock_time: &LockTime) -> u32 {
    use crate::serialization::ZcashSerialize;
    let mut buf = Vec::with_capacity(4);
    lock_time
        .zcash_serialize(&mut buf)
        .expect("serializing LockTime to vec should not fail");
    u32::from_le_bytes(
        buf.try_into()
            .expect("LockTime serializes to exactly 4 bytes"),
    )
}

// ── Height ───────────────────────────────────────────────────────────

/// Convert a Zebra `block::Height` into a librustzcash `BlockHeight`.
#[cfg(any(test, feature = "proptest-impl"))]
pub fn height_to_block_height(h: block::Height) -> zcash_protocol::consensus::BlockHeight {
    h.into()
}

/// Returns an error if the height is out of the valid Zebra range.
pub fn block_height_to_height(
    bh: zcash_protocol::consensus::BlockHeight,
) -> Result<block::Height, SerializationError> {
    block::Height::try_from(bh)
        .map_err(|_| SerializationError::Parse("block height out of valid range"))
}

// ── NetworkUpgrade / BranchId ────────────────────────────────────────

/// Convert a librustzcash `BranchId` into a Zebra `NetworkUpgrade`.
///
/// Returns `None` for unknown branch IDs (e.g., `Sprout` which has branch ID 0).
pub(crate) fn branch_id_to_network_upgrade(
    branch_id: zcash_protocol::consensus::BranchId,
) -> Option<crate::parameters::NetworkUpgrade> {
    crate::parameters::NetworkUpgrade::try_from(u32::from(branch_id)).ok()
}

// ── Sprout JoinSplit fields not exposed by `zcash_primitives` ─────────

/// The number of note ciphertexts in a Sprout JoinSplit description.
const ZC_NUM_JS_OUTPUTS: usize = 2;

/// The size of one Sprout note ciphertext.
pub const SPROUT_CIPHERTEXT_SIZE: usize = 601;

/// The size of a Groth16 Sprout proof (v4 JoinSplits).
const GROTH_PROOF_SIZE: usize = 48 + 96 + 48;

/// The size of a PHGR13 Sprout proof (v2 and v3 JoinSplits).
const PHGR_PROOF_SIZE: usize = 33 + 33 + 65 + 33 + 33 + 33 + 33 + 33;

/// The offset of `ephemeralKey` within a serialized JoinSplit description:
/// `vpub_old` and `vpub_new` (8 bytes each), `anchor`, two `nullifiers`, two `commitments`.
const JS_EPHEMERAL_KEY_OFFSET: usize = 8 + 8 + 32 + (2 * 32) + (2 * 32);

/// The offset of `anchor`, used to self-check the layout below.
const JS_ANCHOR_OFFSET: usize = 8 + 8;

/// Returns the `ephemeralKey` and `encCiphertexts` of a Sprout JoinSplit description.
///
/// [`JsDescription`](zcash_primitives::transaction::components::sprout::JsDescription) exposes
/// accessors for its other fields, but keeps these two private, so they are read back out of its
/// serialization. If upstream gains accessors for them, this can be deleted.
///
/// The returned values are in wire order; callers that render them for RPCs must reverse them,
/// as they do for every other 32-byte JoinSplit field.
pub fn sprout_joinsplit_key_and_ciphertexts(
    joinsplit: &zcash_primitives::transaction::components::sprout::JsDescription,
) -> ([u8; 32], [[u8; SPROUT_CIPHERTEXT_SIZE]; ZC_NUM_JS_OUTPUTS]) {
    let mut bytes = Vec::new();
    joinsplit
        .write(&mut bytes)
        .expect("writing a JoinSplit to a vec cannot fail");

    // Guard the offsets against an upstream layout change: `anchor` sits immediately before the
    // fields read below, and is the last one reachable through a public accessor.
    debug_assert_eq!(
        &bytes[JS_ANCHOR_OFFSET..JS_ANCHOR_OFFSET + 32],
        &joinsplit.anchor()[..],
        "the JoinSplit wire layout must match the offsets used here",
    );

    let mut ephemeral_key = [0u8; 32];
    ephemeral_key.copy_from_slice(&bytes[JS_EPHEMERAL_KEY_OFFSET..JS_EPHEMERAL_KEY_OFFSET + 32]);

    // `ephemeralKey`, `randomSeed`, then two `vmacs`, then the proof.
    let proof_size = if joinsplit.groth_proof_bytes().is_some() {
        GROTH_PROOF_SIZE
    } else {
        PHGR_PROOF_SIZE
    };
    let ciphertexts_offset = JS_EPHEMERAL_KEY_OFFSET + 32 + 32 + (2 * 32) + proof_size;

    let mut ciphertexts = [[0u8; SPROUT_CIPHERTEXT_SIZE]; ZC_NUM_JS_OUTPUTS];
    for (i, ciphertext) in ciphertexts.iter_mut().enumerate() {
        let start = ciphertexts_offset + i * SPROUT_CIPHERTEXT_SIZE;
        ciphertext.copy_from_slice(&bytes[start..start + SPROUT_CIPHERTEXT_SIZE]);
    }

    (ephemeral_key, ciphertexts)
}

// ── Rebuilding transaction data ──────────────────────────────────────

/// The transparent bundle of an authorized transaction.
type TransparentBundle = zcash_transparent::bundle::Bundle<zcash_transparent::bundle::Authorized>;

/// The Sapling bundle of an authorized transaction.
type SaplingBundle =
    sapling_crypto::Bundle<sapling_crypto::bundle::Authorized, zcash_protocol::value::ZatBalance>;

/// An Orchard-shaped bundle of an authorized transaction, used by both the Orchard and
/// Ironwood pools.
type OrchardBundle =
    orchard::Bundle<orchard::bundle::Authorized, zcash_protocol::value::ZatBalance>;

/// Builds [`TransactionData`] from its parts, routing v6 transactions through
/// [`TransactionData::from_parts_v6`].
///
/// Use this instead of [`TransactionData::from_parts`] whenever an existing transaction is being
/// rebuilt. `from_parts` hard-codes `ironwood_bundle: None`, so rebuilding a v6 transaction
/// through it silently drops the Ironwood bundle. That changes the transaction ID, and if the
/// result reaches the verifier it also skips the Ironwood proof check, because the verifier only
/// queues that check when the bundle is present.
///
/// Taking `ironwood_bundle` as a required argument is the point: the compiler will not let a
/// caller forget to carry it across. This is the only place the version dispatch is made.
///
/// [`TransactionData`]: zcash_primitives::transaction::TransactionData
/// [`TransactionData::from_parts`]: zcash_primitives::transaction::TransactionData::from_parts
/// [`TransactionData::from_parts_v6`]: zcash_primitives::transaction::TransactionData::from_parts_v6
#[allow(clippy::too_many_arguments)]
pub(crate) fn transaction_data_from_parts(
    version: zp_tx::TxVersion,
    branch_id: zcash_protocol::consensus::BranchId,
    lock_time: u32,
    expiry_height: zcash_protocol::consensus::BlockHeight,
    transparent_bundle: Option<TransparentBundle>,
    sprout_bundle: Option<zp_tx::components::sprout::Bundle>,
    sapling_bundle: Option<SaplingBundle>,
    orchard_bundle: Option<OrchardBundle>,
    ironwood_bundle: Option<OrchardBundle>,
) -> zp_tx::TransactionData<zp_tx::Authorized> {
    if version == zp_tx::TxVersion::V6 {
        // v6 has no Sprout component, so `sprout_bundle` is intentionally dropped here.
        zp_tx::TransactionData::from_parts_v6(
            branch_id,
            lock_time,
            expiry_height,
            transparent_bundle,
            sapling_bundle,
            orchard_bundle,
            ironwood_bundle,
        )
    } else {
        debug_assert!(
            ironwood_bundle.is_none(),
            "only v6 transactions can carry an Ironwood bundle",
        );

        zp_tx::TransactionData::from_parts(
            version,
            branch_id,
            lock_time,
            expiry_height,
            transparent_bundle,
            sprout_bundle,
            sapling_bundle,
            orchard_bundle,
        )
    }
}
