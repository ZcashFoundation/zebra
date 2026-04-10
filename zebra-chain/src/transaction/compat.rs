//! Conversions between Zebra and `zcash_primitives` transaction types.

use zcash_protocol::value::Zatoshis;

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
        let script_bytes = txin.script_sig().0 .0.clone();
        let (height, data) = crate::transparent::serialize::parse_coinbase_height(script_bytes)?;
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
        transparent::Input::Coinbase {
            height,
            data,
            sequence,
        } => {
            // Serialize height + data into the script_sig, matching the wire format.
            let mut script_bytes = Vec::new();
            crate::transparent::serialize::write_coinbase_height(*height, data, &mut script_bytes)
                .expect("writing coinbase height to vec should not fail");
            script_bytes.extend(data.as_ref());

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
pub fn height_to_block_height(h: block::Height) -> zcash_primitives::consensus::BlockHeight {
    h.into()
}

/// Returns an error if the height is out of the valid Zebra range.
pub fn block_height_to_height(
    bh: zcash_primitives::consensus::BlockHeight,
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
