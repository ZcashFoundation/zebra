//! Serialization and deserialization for Zcash blocks.

use std::{
    borrow::Borrow,
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

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
    transaction::{Transaction, MIN_TRANSPARENT_TX_SIZE},
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
            transactions: zcash_deserialize_block_transactions(limited_reader)?,
        })
    }
}

/// Deserializes a block's transactions, rejecting them once they average fewer than
/// [`MIN_TRANSPARENT_TX_SIZE`] bytes, so a small message can't parse into many transactions.
fn zcash_deserialize_block_transactions<R: io::Read>(
    reader: &mut io::Take<R>,
) -> Result<Vec<Arc<Transaction>>, SerializationError> {
    let count: CompactSizeMessage = (&mut *reader).zcash_deserialize_into()?;
    let count = u64::try_from(usize::from(count))
        .map_err(|_| SerializationError::Parse("transaction count doesn't fit in a u64"))?;
    if count > Transaction::max_allocation() {
        return Err(SerializationError::Parse(
            "Vector longer than max_allocation",
        ));
    }

    let start = reader.limit();
    let mut transactions = Vec::new();
    for parsed in 1..=count {
        transactions.push((&mut *reader).zcash_deserialize_into()?);

        check_transactions_size(parsed, start - reader.limit())?;
    }

    Ok(transactions)
}

/// Returns an error if the first `parsed` transactions of a block, which take up `bytes` bytes,
/// average fewer bytes than the smallest valid transaction.
fn check_transactions_size(parsed: u64, bytes: u64) -> Result<(), SerializationError> {
    if bytes < parsed * MIN_TRANSPARENT_TX_SIZE {
        return Err(SerializationError::Parse(
            "block transactions are smaller than the smallest valid transaction",
        ));
    }

    Ok(())
}

/// Set while a block's transactions are being decoded in parallel.
///
/// See [`Block::zcash_deserialize_parallel`] for why only one block is.
pub(super) static PARALLEL_DECODE: AtomicBool = AtomicBool::new(false);

/// Marks a block as the one that is being decoded in parallel, until it is dropped.
pub(super) struct ParallelDecode<'a>(&'a AtomicBool);

impl<'a> ParallelDecode<'a> {
    /// Sets `decoding` and returns its mark, or returns `None` if it is already set.
    pub(super) fn try_start(decoding: &'a AtomicBool) -> Option<Self> {
        decoding
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(decoding))
    }
}

impl Drop for ParallelDecode<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Block {
    /// Decodes a byte slice, parsing and hashing large blocks' transactions on Rayon.
    ///
    /// Small blocks use the serial decoder. Large blocks use a layout-only boundary scan,
    /// then the canonical transaction decoder in bounded batches. Errors and consumed bytes,
    /// including trailing bytes and the block-size limit, match the serial decoder.
    ///
    /// Only one block at a time is decoded in parallel, across all callers. A large block that
    /// arrives during that decode uses the serial decoder, as it does without this function.
    /// A Rayon worker that waits for the tasks of a parallel decode can start another queued
    /// job. If that job could decode a block in parallel too, a caller that decodes each block
    /// in a Rayon job would nest those decodes on the worker's stack. This way a worker has at
    /// most one serial decode on top of a parallel one.
    pub fn zcash_deserialize_parallel(bytes: &mut &[u8]) -> Result<Self, SerializationError> {
        // The mark is never waited for, so this can't block a Rayon worker. A small block only
        // holds it while its transaction count is read.
        let decoded =
            ParallelDecode::try_start(&PARALLEL_DECODE).and_then(|_decoding| parallel_block(bytes));

        if let Some((block, consumed)) = decoded {
            *bytes = &bytes[consumed..];
            block
        } else {
            Self::zcash_deserialize(bytes)
        }
    }
}

/// The minimum transaction bytes each Rayon task parses.
///
/// Handing work to another thread can cost tens of microseconds, more than parsing a small
/// transaction, so tiny tasks make parallel decoding slower than serial decoding.
pub(super) const PARALLEL_DECODE_TASK_BYTES: usize = 32 * 1024;

/// The serial prefix ends after this many accepted bytes, even before 64 transactions, so a
/// block of large transactions starts parallel decoding sooner.
pub(super) const PARALLEL_DECODE_BATCH_BYTES: usize = 64 * 1024;

const PARALLEL_DECODE_THRESHOLD: usize = 64;

/// Finishes the canonical prefix by transaction count or accepted bytes, whichever comes first.
pub(super) fn serial_decode_prefix_is_complete(transactions: usize, bytes: usize) -> bool {
    transactions >= PARALLEL_DECODE_THRESHOLD || bytes >= PARALLEL_DECODE_BATCH_BYTES
}

/// Runs the serial decoder's transaction size check on the `transactions` accepted so far, which
/// take up `bytes` bytes, so both decoders reject a block at the same transaction.
fn check_accepted_size(transactions: usize, bytes: usize) -> Result<(), SerializationError> {
    check_transactions_size(u64::try_from(transactions)?, u64::try_from(bytes)?)
}

/// Decodes a serial prefix before bounded speculative work.
///
/// Each batch is limited to the bytes and transactions accepted so far, so batches double
/// in size, and an invalid block costs at most about twice the work of its valid prefix.
/// Each speculative parse only reads its scanned bytes, so that bound holds even where the
/// scan and the decoder disagree about a transaction's length.
pub(super) fn parallel_block(bytes: &[u8]) -> Option<(Result<Block, SerializationError>, usize)> {
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
            // The canonical prefix is bounded by count and bytes. Later speculation never
            // exceeds the already accepted wire bytes or transactions.
            if serial
                || !serial_decode_prefix_is_complete(transactions.len(), consumed - body_start)
            {
                let mut reader = &bounded[consumed..];
                let transaction = Transaction::zcash_deserialize(&mut reader);
                consumed = bounded.len() - reader.len();
                transactions.push(Arc::new(transaction?));
                check_accepted_size(transactions.len(), consumed - body_start)?;
                continue;
            }
            let start = consumed;
            // Cut the scan to the budget, so a huge transaction fails the scan quickly.
            let budget_end = bounded.len().min(start + (consumed - body_start));
            scan_batch(
                &bounded[start..budget_end],
                (count - transactions.len()).min(transactions.len()),
                &mut ranges,
            );
            let Some(last) = ranges.last() else {
                // The next transaction is malformed or larger than the accepted prefix.
                let mut reader = &bounded[consumed..];
                let transaction = Transaction::zcash_deserialize(&mut reader);
                consumed = bounded.len() - reader.len();
                transactions.push(Arc::new(transaction?));
                check_accepted_size(transactions.len(), consumed - body_start)?;
                continue;
            };
            // Divides the batch into tasks of about `PARALLEL_DECODE_TASK_BYTES`.
            let task_len = PARALLEL_DECODE_TASK_BYTES
                .saturating_mul(ranges.len())
                .div_ceil(last.end)
                .max(1);
            let first_miss = AtomicUsize::new(usize::MAX);
            ranges
                .par_iter()
                .enumerate()
                .with_min_len(task_len)
                .map(|(index, range)| {
                    // Don't parse past a miss: those results are never used.
                    if index > first_miss.load(Ordering::Relaxed) {
                        return None;
                    }
                    // Only read the scanned bytes, so a stale boundary can't make every later
                    // task in the batch parse the rest of the block.
                    let mut reader = &bounded[start + range.start..start + range.end];
                    let transaction = Transaction::zcash_deserialize(&mut reader)
                        .ok()
                        .filter(|_| reader.is_empty());
                    if transaction.is_none() {
                        first_miss.fetch_min(index, Ordering::Relaxed);
                    }
                    Some(transaction.map(Arc::new))
                })
                .collect_into_vec(&mut parsed);
            for (result, range) in parsed.drain(..).zip(&ranges) {
                let transaction = result.expect(
                    "only transactions after a miss are skipped, and the loop stops at misses",
                );
                let Some(transaction) = transaction else {
                    // An error, or a transaction that doesn't end where the scan ends, is
                    // decoded again from the rest of the block, which gives the serial
                    // decoder's transaction or error, and its consumed bytes.
                    serial = true;
                    break;
                };
                consumed = start + range.end;
                transactions.push(transaction);
                check_accepted_size(transactions.len(), consumed - body_start)?;
            }
        }
        Ok(Block {
            header,
            transactions,
        })
    })();
    Some((block, consumed))
}

/// Scans up to `count` transaction boundaries in `bytes`, stopping at the first
/// transaction that does not fit or does not scan.
pub(super) fn scan_batch(bytes: &[u8], count: usize, ranges: &mut Vec<std::ops::Range<usize>>) {
    ranges.clear();
    let mut remaining = bytes;
    while ranges.len() < count {
        let start = bytes.len() - remaining.len();
        if scan_transaction(&mut remaining).is_none() {
            break;
        }
        ranges.push(start..bytes.len() - remaining.len());
    }
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

impl SerializedBlock {
    /// Consumes the serialized block, returning the underlying bytes without copying.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
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
