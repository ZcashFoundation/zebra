//! Serialization and deserialization for Zcash blocks.

use std::{borrow::Borrow, io, sync::Arc};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{TimeZone, Utc};
use hex::{FromHex, FromHexError};
use rayon::prelude::*;
use zcash_primitives::transaction::TxVersion;

use crate::{
    block::{header::ZCASH_BLOCK_VERSION, merkle, Block, CountedHeader, Hash, Header},
    serialization::{
        CompactSizeMessage, ReadZcashExt, SerializationError, TrustedPreallocate, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
    transaction::Transaction,
    work::{difficulty::CompactDifficulty, equihash},
};

/// The maximum size of a Zcash block, in bytes.
///
/// Post-Sapling, this is also the maximum size of a transaction
/// in the Zcash specification. (But since blocks also contain a
/// block header and transaction count, the maximum size of a
/// transaction in the chain is approximately 1.5 kB smaller.)
pub const MAX_BLOCK_BYTES: u64 = 2_000_000;

/// Checks if a block header version is valid.
///
/// Zebra could encounter a [`Header`] with an invalid version when serializing a block header constructed
/// in memory with the wrong version in tests or the getblocktemplate RPC.
///
/// The getblocktemplate RPC generates a template with version 4. The miner generates the actual block,
/// and then we deserialize it and do this check.
///
/// All other blocks are deserialized when we receive them, and never modified,
/// so the deserialisation would pick up any errors.
fn check_version(version: u32) -> Result<(), &'static str> {
    match version {
        // The Zcash specification says that:
        // "The current and only defined block version number for Zcash is 4."
        // but this is not actually part of the consensus rules, and in fact
        // broken mining software created blocks that do not have version 4.
        // There are approximately 4,000 blocks with version 536870912; this
        // is the bit-reversal of the value 4, indicating that mining pool
        // reversed bit-ordering of the version field. Because the version field
        // was not properly validated, these blocks were added to the chain.
        //
        // The only possible way to work around this is to do a similar hack
        // as the overwintered field in transaction parsing, which we do here:
        // treat the high bit (which zcashd interprets as a sign bit) as an
        // indicator that the version field is meaningful.
        version if version >> 31 != 0 => Err("high bit was set in version field"),

        // # Consensus
        //
        // > The block version number MUST be greater than or equal to 4.
        //
        // https://zips.z.cash/protocol/protocol.pdf#blockheader
        version if version < ZCASH_BLOCK_VERSION => Err("version must be at least 4"),

        _ => Ok(()),
    }
}

impl ZcashSerialize for Header {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        check_version(self.version).map_err(io::Error::other)?;

        writer.write_u32::<LittleEndian>(self.version)?;
        self.previous_block_hash.zcash_serialize(&mut writer)?;
        writer.write_all(&self.merkle_root.0[..])?;
        writer.write_all(&self.commitment_bytes[..])?;
        writer.write_u32::<LittleEndian>(
            self.time
                .timestamp()
                .try_into()
                .expect("deserialized and generated timestamps are u32 values"),
        )?;
        writer.write_u32::<LittleEndian>(self.difficulty_threshold.0)?;
        writer.write_all(&self.nonce[..])?;
        self.solution.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Header {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let version = reader.read_u32::<LittleEndian>()?;
        check_version(version).map_err(SerializationError::Parse)?;

        Ok(Header {
            version,
            previous_block_hash: Hash::zcash_deserialize(&mut reader)?,
            merkle_root: merkle::Root(reader.read_32_bytes()?),
            commitment_bytes: reader.read_32_bytes()?.into(),
            // This can't panic, because all u32 values are valid `Utc.timestamp`s
            time: Utc
                .timestamp_opt(reader.read_u32::<LittleEndian>()?.into(), 0)
                .single()
                .ok_or(SerializationError::Parse(
                    "out-of-range number of seconds and/or invalid nanosecond",
                ))?,
            difficulty_threshold: CompactDifficulty(reader.read_u32::<LittleEndian>()?),
            nonce: reader.read_32_bytes()?.into(),
            solution: equihash::Solution::zcash_deserialize(reader)?,
        })
    }
}

impl ZcashSerialize for CountedHeader {
    #[allow(clippy::unwrap_in_result)]
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.header.zcash_serialize(&mut writer)?;

        // A header-only message has zero transactions in it.
        let transaction_count =
            CompactSizeMessage::try_from(0).expect("0 is below the message size limit");
        transaction_count.zcash_serialize(&mut writer)?;

        Ok(())
    }
}

impl ZcashDeserialize for CountedHeader {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let header = CountedHeader {
            header: (&mut reader).zcash_deserialize_into()?,
        };

        // We ignore the number of transactions in a header-only message,
        // it should always be zero.
        let _transaction_count: CompactSizeMessage = (&mut reader).zcash_deserialize_into()?;

        Ok(header)
    }
}

impl ZcashSerialize for Block {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // All block structs are validated when they are parsed.
        // So we don't need to check MAX_BLOCK_BYTES here, until
        // we start generating our own blocks (see #483).
        self.header.zcash_serialize(&mut writer)?;
        self.transactions.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Block {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        // # Consensus
        //
        // > The size of a block MUST be less than or equal to 2000000 bytes.
        //
        // https://zips.z.cash/protocol/protocol.pdf#blockheader
        //
        // If the limit is reached, we'll get an UnexpectedEof error
        let limited_reader = &mut reader.take(MAX_BLOCK_BYTES);
        Ok(Block {
            header: limited_reader.zcash_deserialize_into()?,
            // Note: transactions are parsed with a default `BranchId::Canopy` for V1-V4.
            // The stored `consensus_branch_id` may be wrong for pre-Canopy transactions.
            // This field is not used in production (sighash computation provides the
            // correct branch ID explicitly via the NetworkUpgrade parameter).
            transactions: limited_reader.zcash_deserialize_into()?,
        })
    }
}

impl Block {
    /// Decodes a byte slice, parsing and hashing large blocks' transactions on Rayon.
    ///
    /// Small blocks use the serial decoder. Large blocks use a layout-only boundary scan,
    /// then the canonical transaction decoder in bounded batches. Errors and consumed bytes,
    /// including trailing bytes and the block-size limit, match the serial decoder.
    pub fn zcash_deserialize_parallel(bytes: &mut &[u8]) -> Result<Self, SerializationError> {
        if let Some((block, consumed)) = parallel_block(bytes) {
            *bytes = &bytes[consumed..];
            block
        } else {
            Self::zcash_deserialize(bytes)
        }
    }
}

/// Limits speculative parsing after an invalid transaction to 64 KiB. Larger transactions
/// occupy a batch alone, so an early error cannot trigger decoding the rest of the block.
pub(super) const PARALLEL_DECODE_BATCH_BYTES: usize = 64 * 1024;

/// Decodes a serial prefix before bounded speculative work; no canonical parse is replayed.
pub(super) fn parallel_block(bytes: &[u8]) -> Option<(Result<Block, SerializationError>, usize)> {
    // Below 64 transactions, Rayon scheduling can outweigh parallel parsing and hashing.
    const PARALLEL_DECODE_THRESHOLD: usize = 64;

    let limit = usize::try_from(MAX_BLOCK_BYTES).expect("block limit fits in usize");
    let bounded = &bytes[..bytes.len().min(limit)];
    // Probe past the fixed header prefix, nonce and variable Equihash solution without
    // allocating a header. Small blocks go straight to the ordinary serial decoder.
    let mut remaining = bounded.get(equihash::Solution::INPUT_LENGTH + 32..)?;
    scan_fixed_vector(&mut remaining, 1)?;
    let count = scan_count(&mut remaining)?;
    if count < PARALLEL_DECODE_THRESHOLD
        || u64::try_from(count).ok()? > Transaction::max_allocation()
    {
        return None;
    }
    let header = bounded.zcash_deserialize_into().ok()?;

    let body_start = bounded.len() - remaining.len();
    let mut consumed = body_start;
    let block = (|| {
        let mut transactions = Vec::new();
        let mut ranges = Vec::new();
        let mut parsed = Vec::new();
        let mut serial = false;
        while transactions.len() < count {
            // Keep early failures as cheap as serial decoding. Later speculation never
            // exceeds the already accepted wire bytes or the fixed batch limit.
            if transactions.len() < PARALLEL_DECODE_THRESHOLD || serial {
                let mut reader = &bounded[consumed..];
                let transaction = Transaction::zcash_deserialize(&mut reader);
                consumed = bounded.len() - reader.len();
                transactions.push(Arc::new(transaction?));
                continue;
            }
            let start = consumed;
            let budget = (consumed - body_start).min(PARALLEL_DECODE_BATCH_BYTES);
            if scan_batch(
                &bounded[start..],
                count - transactions.len(),
                budget,
                &mut ranges,
            )
            .is_none()
            {
                // The scan did not parse anything. Resume once from the accepted prefix.
                serial = true;
                continue;
            }
            ranges
                .par_iter()
                .map(|range| {
                    // Canonical errors and consumption require the full bounded suffix,
                    // even if a future wire layout makes the scan's boundary stale.
                    let mut reader = &bounded[start + range.start..];
                    let transaction = Transaction::zcash_deserialize(&mut reader).map(Arc::new);
                    (transaction, bounded.len() - reader.len())
                })
                .collect_into_vec(&mut parsed);
            for ((transaction, end), range) in parsed.drain(..).zip(&ranges) {
                consumed = end;
                transactions.push(transaction?);
                if end != start + range.end {
                    // Do not use stale boundaries or repeat already accepted parsing.
                    serial = true;
                    break;
                }
            }
        }
        Ok(Block {
            header,
            transactions,
        })
    })();
    Some((block, consumed))
}

/// Scan one bounded batch, keeping an oversized first transaction alone and bounding later scans.
pub(super) fn scan_batch(
    bytes: &[u8],
    count: usize,
    budget: usize,
    ranges: &mut Vec<std::ops::Range<usize>>,
) -> Option<()> {
    ranges.clear();
    let mut remaining = bytes;
    for _ in 0..count {
        let start = bytes.len() - remaining.len();
        let mut reader = remaining;
        if !ranges.is_empty() {
            reader = &reader[..reader.len().min(budget - start)];
        }
        let available = reader.len();
        if scan_transaction(&mut reader).is_none() {
            return if ranges.is_empty() { None } else { Some(()) };
        }
        let end = start + available - reader.len();
        remaining = &bytes[end..];
        ranges.push(start..end);
        if end >= budget {
            break;
        }
    }
    Some(())
}

/// Skips a bounded number of bytes without copying or allocating.
fn skip(bytes: &mut &[u8], count: usize) -> Option<()> {
    *bytes = bytes.get(count..)?;
    Some(())
}

/// Counts use the same canonical CompactSize encoding and message bound as the decoder.
fn scan_count(bytes: &mut &[u8]) -> Option<usize> {
    CompactSizeMessage::zcash_deserialize(bytes)
        .ok()
        .map(usize::from)
}

fn scan_fixed_vector(bytes: &mut &[u8], item_size: usize) -> Option<usize> {
    let count = scan_count(bytes)?;
    skip(bytes, count.checked_mul(item_size)?)?;
    Some(count)
}

/// Wire-layout-only scan of the formats accepted by the pinned native parser.
///
/// Fixed lengths follow the transaction encoding tables in §7.1 of the protocol spec.
/// Field encodings, amounts, branch IDs, proofs, and consensus checks belong exclusively
/// to `Transaction::zcash_deserialize`, never this scanner.
pub(super) fn scan_transaction(bytes: &mut &[u8]) -> Option<()> {
    let version = TxVersion::read(&mut *bytes).ok()?;
    if version.has_orchard() {
        // nConsensusBranchId, nLockTime, nExpiryHeight (V5 and V6).
        skip(bytes, 12)?;
    }
    for _ in 0..scan_count(bytes)? {
        skip(bytes, 36)?; // prevout
        let script_len = scan_count(bytes)?;
        skip(bytes, script_len)?;
        skip(bytes, 4)?; // nSequence
    }
    for _ in 0..scan_count(bytes)? {
        skip(bytes, 8)?; // value
        let script_len = scan_count(bytes)?;
        skip(bytes, script_len)?;
    }
    if !version.has_orchard() {
        skip(bytes, 4)?; // nLockTime
        if version.has_overwinter() {
            skip(bytes, 4)?; // nExpiryHeight
        }
        let mut has_sapling = false;
        if version.has_sapling() {
            skip(bytes, 8)?; // valueBalanceSapling
            let spends = scan_fixed_vector(bytes, 384)?;
            let outputs = scan_fixed_vector(bytes, 948)?;
            has_sapling = spends != 0 || outputs != 0;
        }
        if version.has_sprout() {
            // PHGR13 proofs before V4; Groth16 proofs in V4.
            let size = if version.has_sapling() { 1698 } else { 1802 };
            if scan_fixed_vector(bytes, size)? != 0 {
                skip(bytes, 96)?; // joinSplitPubKey, joinSplitSig
            }
        }
        if has_sapling {
            skip(bytes, 64)?; // bindingSigSapling
        }
    } else {
        let spends = scan_fixed_vector(bytes, 96)?;
        let outputs = scan_fixed_vector(bytes, 756)?;
        if spends != 0 || outputs != 0 {
            skip(bytes, 8)?; // valueBalanceSapling
            if spends != 0 {
                skip(bytes, 32)?; // anchorSapling
            }
            skip(bytes, spends.checked_mul(192 + 64)?)?;
            skip(bytes, outputs.checked_mul(192)?)?;
            skip(bytes, 64)?; // bindingSigSapling
        }
        for _ in 0..if version.has_ironwood() { 2 } else { 1 } {
            let actions = scan_fixed_vector(bytes, 820)?;
            if actions != 0 {
                skip(bytes, 1 + 8 + 32)?; // flags, valueBalance, anchor
                let proof_len = scan_count(bytes)?;
                skip(bytes, proof_len)?;
                skip(bytes, actions.checked_mul(64)?)?;
                skip(bytes, 64)?; // bindingSig
            }
        }
    }
    Some(())
}

/// A serialized block.
///
/// Stores bytes that are guaranteed to be deserializable into a [`Block`].
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SerializedBlock {
    bytes: Vec<u8>,
}

/// Build a [`SerializedBlock`] by serializing a block.
impl<B: Borrow<Block>> From<B> for SerializedBlock {
    fn from(block: B) -> Self {
        SerializedBlock {
            bytes: block
                .borrow()
                .zcash_serialize_to_vec()
                .expect("Writing to a `Vec` should never fail"),
        }
    }
}

/// Access the serialized bytes of a [`SerializedBlock`].
impl AsRef<[u8]> for SerializedBlock {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

impl From<Vec<u8>> for SerializedBlock {
    fn from(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl FromHex for SerializedBlock {
    type Error = FromHexError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let bytes = Vec::from_hex(hex)?;
        Ok(SerializedBlock { bytes })
    }
}
