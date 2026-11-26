//! Provisional compact relay encoding version 1, requiring both peers to opt in.
//!
//! Short IDs commit to witnessed identities, not just transaction effects. Reconstruction checks
//! size, ordering, transaction Merkle roots and duplicates; callers must authenticate authorizing
//! data against the committed tip before submitting the resulting block to consensus. Hashing and
//! reconstruction are synchronous and must run on a blocking worker.

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    fmt,
    hash::Hasher,
    io::{self, Read, Write},
    sync::Arc,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use sha2::{Digest, Sha256};
use siphasher::sip::SipHasher24;
use zebra_chain::{
    block::{self, Block, MAX_BLOCK_BYTES},
    serialization::{
        CompactSizeMessage, FakeWriter, SerializationError, TrustedPreallocate, ZcashDeserialize,
        ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN,
    },
    transaction::{Transaction, UnminedTx, UnminedTxId, MIN_TRANSPARENT_TX_SIZE},
};

#[cfg(test)]
pub(crate) mod tests;

/// A full Zcash header and ordered compact transaction identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactBlock {
    /// The complete header, including its Equihash solution.
    pub header: Arc<block::Header>,
    /// Nonce used with the serialized header to derive the short-ID keys.
    pub nonce: u64,
    /// Six-byte identities, in block order, omitting prefilled positions.
    pub short_ids: Vec<[u8; 6]>,
    /// Strictly increasing absolute positions and complete transactions, including coinbase at zero.
    pub prefilled: Vec<(usize, Arc<Transaction>)>,
}

/// A single recovery request for missing absolute transaction positions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockTransactionsRequest {
    /// Hash of the advertised block.
    pub block_hash: block::Hash,
    /// Strictly increasing absolute indexes, encoded differentially on the wire.
    pub indexes: Vec<usize>,
}

/// Complete transactions in the corresponding recovery request's order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockTransactions {
    /// Hash of the advertised block.
    pub block_hash: block::Hash,
    /// Exactly one transaction for each requested position.
    pub transactions: Vec<Arc<Transaction>>,
}

/// The `blocktxn` response that a connection is waiting for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExpectedBlockTransactions {
    /// The hash of the block the transactions are for.
    pub(crate) block_hash: block::Hash,

    /// The number of transactions the connection asked for.
    pub(crate) count: usize,

    /// When the connection stops waiting for them.
    pub(crate) deadline: tokio::time::Instant,
}

impl ExpectedBlockTransactions {
    /// Returns `true` if `body`, the body of a `blocktxn` message, starts with the expected
    /// block hash and transaction count, and the connection is still waiting for it.
    ///
    /// This only reads the hash and the count, so an unexpected response isn't decoded.
    pub(crate) fn matches(&self, mut body: &[u8]) -> bool {
        tokio::time::Instant::now() < self.deadline
            && block::Hash::zcash_deserialize(&mut body).ok() == Some(self.block_hash)
            && CompactSizeMessage::zcash_deserialize(&mut body)
                .ok()
                .map(usize::from)
                == Some(self.count)
    }
}

/// Bounded partial reconstruction; not a consensus or authorizing-data validation verdict.
pub struct Reconstruction {
    compact: Arc<CompactBlock>,
    transactions: Vec<Option<Arc<Transaction>>>,
}

impl fmt::Debug for Reconstruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A connection logs its state, which can hold a reconstruction. So this leaves out the
        // transactions, which can be as large as a block, and can come from the mempool.
        f.debug_struct("Reconstruction")
            .field("transactions", &self.transactions.len())
            .field("missing", &self.missing_count())
            .finish()
    }
}

impl CompactBlock {
    /// Builds an announcement with only the coinbase prefilled.
    ///
    /// Returns an error if the block doesn't encode, or if the announcement would fail
    /// [`Self::validate`], so every announcement this returns can be serialized. The caller must
    /// use ordinary block relay if this returns an error, or if this encoding would not save bytes.
    pub fn from_block(block: &Block, nonce: u64) -> Result<Self, SerializationError> {
        check_count(block.transactions.len())?;
        let mut block_bytes = FakeWriter(0);
        block.zcash_serialize(&mut block_bytes)?;
        check_bytes(block_bytes.0)?;
        let coinbase = block
            .transactions
            .first()
            .filter(|tx| tx.is_coinbase())
            .ok_or(SerializationError::Parse("compact block requires coinbase"))?;
        let mut compact = Self {
            header: block.header.clone(),
            nonce,
            short_ids: Vec::with_capacity(block.transactions.len() - 1),
            prefilled: vec![(0, coinbase.clone())],
        };
        let keys = compact.keys();
        compact.short_ids.extend(
            block
                .transactions
                .iter()
                .skip(1)
                .map(|tx| short_id(keys, tx.as_ref().into())),
        );
        compact.validate()?;
        Ok(compact)
    }

    /// Checks count, prefilled ordering, coinbase, and retained transaction byte bounds.
    pub fn validate(&self) -> Result<(), SerializationError> {
        let count = self
            .short_ids
            .len()
            .checked_add(self.prefilled.len())
            .ok_or(SerializationError::Parse(
                "compact transaction count overflow",
            ))?;
        check_count(count)?;
        if !self
            .prefilled
            .first()
            .is_some_and(|(index, tx)| *index == 0 && tx.is_coinbase())
        {
            return Err(SerializationError::Parse(
                "compact block requires prefilled coinbase at zero",
            ));
        }
        let mut next = 0;
        let mut bytes = self.header.zcash_serialized_size()
            + CompactSizeMessage::try_from(count)?.zcash_serialized_size()
            + self.short_ids.len() * usize::try_from(MIN_TRANSPARENT_TX_SIZE)?;
        for (index, tx) in &self.prefilled {
            if *index < next || *index >= count || (*index != 0 && tx.is_coinbase()) {
                return Err(SerializationError::Parse(
                    "invalid compact prefilled index or coinbase",
                ));
            }
            next = index + 1;
            bytes = add_bytes(bytes, tx.zcash_serialized_size())?;
        }
        check_bytes(bytes)
    }

    /// Returns the block identity committed to by the full header.
    pub fn block_hash(&self) -> block::Hash {
        self.header.hash()
    }

    /// Scans identifiers once, retaining only unique matches to non-repeated advertised short IDs.
    /// Identical full identities are deduplicated; distinct colliding identities remain missing.
    pub fn matching_ids(&self, ids: impl IntoIterator<Item = UnminedTxId>) -> HashSet<UnminedTxId> {
        let keys = self.keys();
        self.matching_short_ids(ids.into_iter().map(|id| (short_id(keys, id), id)))
    }

    fn matching_short_ids(
        &self,
        ids: impl Iterator<Item = ([u8; 6], UnminedTxId)>,
    ) -> HashSet<UnminedTxId> {
        enum Match {
            Missing,
            Unique(UnminedTxId),
            Ambiguous,
        }
        let mut matches = HashMap::with_capacity(self.short_ids.len());
        for id in &self.short_ids {
            match matches.entry(*id) {
                Entry::Vacant(entry) => {
                    entry.insert(Match::Missing);
                }
                Entry::Occupied(mut entry) => {
                    entry.insert(Match::Ambiguous);
                }
            }
        }
        for (short, id) in ids {
            if let Some(matched) = matches.get_mut(&short) {
                match matched {
                    Match::Missing => *matched = Match::Unique(id),
                    Match::Unique(previous) if *previous != id => *matched = Match::Ambiguous,
                    _ => {}
                }
            }
        }
        matches
            .into_values()
            .filter_map(|matched| match matched {
                Match::Unique(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// Places bounded candidate bodies without accepting ambiguous short-ID matches.
    /// Candidate cached identities and sizes must come from locally constructed [`UnminedTx`]s.
    #[allow(clippy::unwrap_in_result)]
    pub fn reconstruct(
        self: Arc<Self>,
        candidates: Vec<UnminedTx>,
    ) -> Result<Reconstruction, SerializationError> {
        self.validate()?;
        check_count(candidates.len())?;
        candidates
            .iter()
            .try_fold(0, |bytes, tx| add_bytes(bytes, tx.size))?;
        let keys = self.keys();
        let mut matching =
            self.matching_short_ids(candidates.iter().map(|tx| (short_id(keys, tx.id), tx.id)));
        let mut bodies: HashMap<_, _> = candidates
            .into_iter()
            .filter(|tx| matching.remove(&tx.id))
            .map(|tx| (short_id(keys, tx.id), tx.transaction))
            .collect();
        let mut transactions = vec![None; self.short_ids.len() + self.prefilled.len()];
        for (index, tx) in &self.prefilled {
            transactions[*index] = Some(tx.clone());
        }
        let mut short_ids = self.short_ids.iter();
        for slot in &mut transactions {
            if slot.is_none() {
                *slot = bodies.remove(
                    short_ids
                        .next()
                        .expect("each non-prefilled slot has a short ID"),
                );
            }
        }
        let bytes = self.header.zcash_serialized_size()
            + CompactSizeMessage::try_from(transactions.len())?.zcash_serialized_size();
        transactions.iter().flatten().try_fold(bytes, |bytes, tx| {
            add_bytes(bytes, tx.zcash_serialized_size())
        })?;
        Ok(Reconstruction {
            compact: self,
            transactions,
        })
    }

    fn keys(&self) -> (u64, u64) {
        let mut hash = Sha256::new();
        self.header
            .zcash_serialize(&mut hash)
            .expect("SHA256 writer cannot fail");
        hash.update(self.nonce.to_le_bytes());
        let digest = hash.finalize();
        (
            u64::from_le_bytes(digest[..8].try_into().expect("eight-byte key")),
            u64::from_le_bytes(digest[8..16].try_into().expect("eight-byte key")),
        )
    }
}

impl Reconstruction {
    /// Returns the number of transactions in the block.
    pub fn transaction_count(&self) -> usize {
        self.transactions.len()
    }

    /// Returns the number of transactions that are still missing.
    pub fn missing_count(&self) -> usize {
        self.transactions.iter().filter(|tx| tx.is_none()).count()
    }

    /// Returns unresolved absolute indexes in the original block order.
    pub fn missing_indexes(&self) -> Vec<usize> {
        self.transactions
            .iter()
            .enumerate()
            .filter_map(|(index, tx)| tx.is_none().then_some(index))
            .collect()
    }

    /// Returns the hash expected in the recovery response.
    pub fn block_hash(&self) -> block::Hash {
        self.compact.block_hash()
    }

    /// Completes reconstruction with exactly the requested bodies, including an empty response
    /// when all transactions were available locally. Errors require full-block fallback, not a ban.
    /// Authorizing-data commitments and normal consensus verification are still required.
    #[allow(clippy::unwrap_in_result)]
    pub fn finish(mut self, response: BlockTransactions) -> Result<Arc<Block>, SerializationError> {
        if response.block_hash != self.block_hash()
            || response.transactions.len()
                != self.transactions.iter().filter(|tx| tx.is_none()).count()
        {
            return Err(SerializationError::Parse(
                "compact recovery hash or count mismatch",
            ));
        }
        let bytes = self.compact.header.zcash_serialized_size()
            + CompactSizeMessage::try_from(self.transactions.len())?.zcash_serialized_size();
        self.transactions
            .iter()
            .flatten()
            .chain(response.transactions.iter())
            .try_fold(bytes, |bytes, tx| {
                add_bytes(bytes, tx.zcash_serialized_size())
            })?;
        self.fill_missing(response)?;
        let block = Arc::new(Block {
            header: self.compact.header.clone(),
            transactions: self
                .transactions
                .into_iter()
                .map(|tx| tx.expect("every missing slot was filled by the exact response count"))
                .collect(),
        });
        let mut seen = HashSet::with_capacity(block.transactions.len());
        if block.transactions.iter().any(|tx| !seen.insert(tx.hash()))
            || block.transactions.iter().collect::<block::merkle::Root>()
                != block.header.merkle_root
        {
            return Err(SerializationError::Parse(
                "compact block duplicate transaction or Merkle mismatch",
            ));
        }
        Ok(block)
    }

    /// Places the transactions of `response` in the missing slots, in block order, checking each
    /// against its slot's short ID. `finish` has already checked that `response` has one
    /// transaction for each missing slot.
    #[allow(clippy::unwrap_in_result)]
    fn fill_missing(&mut self, response: BlockTransactions) -> Result<(), SerializationError> {
        let keys = self.compact.keys();
        let mut recovered = response.transactions.into_iter();
        let mut prefilled = self.compact.prefilled.iter().peekable();
        let mut short_ids = self.compact.short_ids.iter();
        for (index, slot) in self.transactions.iter_mut().enumerate() {
            if prefilled
                .peek()
                .is_some_and(|(position, _)| *position == index)
            {
                prefilled.next();
                continue;
            }
            let expected = short_ids
                .next()
                .expect("each non-prefilled slot has a short ID");
            if slot.is_some() {
                continue;
            }
            let tx = recovered
                .next()
                .expect("response count equals missing slot count");
            if short_id(keys, tx.as_ref().into()) != *expected {
                return Err(SerializationError::Parse(
                    "compact recovery identity or order mismatch",
                ));
            }
            *slot = Some(tx);
        }
        Ok(())
    }
}

fn short_id(keys: (u64, u64), id: UnminedTxId) -> [u8; 6] {
    let mut hash = SipHasher24::new_with_keys(keys.0, keys.1);
    match id {
        UnminedTxId::Legacy(id) => hash.write(&id.0),
        UnminedTxId::Witnessed(id) => hash.write(&<[u8; 64]>::from(id)),
    }
    hash.finish().to_le_bytes()[..6]
        .try_into()
        .expect("six-byte short ID")
}

fn check_count(count: usize) -> Result<(), SerializationError> {
    if u64::try_from(count)? > Transaction::max_allocation() {
        return Err(SerializationError::Parse(
            "compact transaction count exceeds block limit",
        ));
    }
    Ok(())
}

fn check_bytes(bytes: usize) -> Result<(), SerializationError> {
    if u64::try_from(bytes)? > MAX_BLOCK_BYTES {
        return Err(SerializationError::Parse(
            "compact transaction bytes exceed block limit",
        ));
    }
    Ok(())
}

/// Returns an error if the first `parsed` transactions of a message, which take up `bytes`
/// bytes, average fewer bytes than the smallest valid transaction. The block decoder has the
/// same rule for a block's transactions.
fn check_transactions_size(parsed: usize, bytes: u64) -> Result<(), SerializationError> {
    if bytes < u64::try_from(parsed)?.saturating_mul(MIN_TRANSPARENT_TX_SIZE) {
        return Err(SerializationError::Parse(
            "compact transactions are smaller than the smallest valid transaction",
        ));
    }

    Ok(())
}

fn add_bytes(bytes: usize, extra: usize) -> Result<usize, SerializationError> {
    let bytes = bytes.checked_add(extra).ok_or(SerializationError::Parse(
        "compact transaction size overflow",
    ))?;
    check_bytes(bytes)?;
    Ok(bytes)
}

fn read_count(reader: &mut impl Read) -> Result<usize, SerializationError> {
    let count = usize::from(CompactSizeMessage::zcash_deserialize(reader)?);
    check_count(count)?;
    Ok(count)
}

/// The largest number of items that memory is reserved for before any of them is read.
///
/// A peer chooses a message's counts, and a short message can claim the largest count. So a
/// vector only grows past this as its items arrive, like the vectors that `zebra-chain` reads.
const MAX_INITIAL_ALLOCATION: usize = 1024;

/// Returns an empty vector for the `count` items that a peer's message says it has.
fn vec_for_count<T>(count: usize) -> Vec<T> {
    Vec::with_capacity(count.min(MAX_INITIAL_ALLOCATION))
}

fn read_index(
    reader: &mut impl Read,
    next: &mut usize,
    count: usize,
) -> Result<usize, SerializationError> {
    let delta = usize::from(CompactSizeMessage::zcash_deserialize(reader)?);
    let index = next
        .checked_add(delta)
        .filter(|index| *index < count)
        .ok_or(SerializationError::Parse(
            "compact differential index exceeds block limit",
        ))?;
    *next = index + 1;
    Ok(index)
}

fn write_count(writer: &mut impl Write, count: usize) -> io::Result<()> {
    CompactSizeMessage::try_from(count)
        .map_err(io::Error::other)?
        .zcash_serialize(writer)
}

fn write_index(writer: &mut impl Write, index: usize, next: &mut usize) -> io::Result<()> {
    let delta = index
        .checked_sub(*next)
        .ok_or_else(|| io::Error::other("unordered compact indexes"))?;
    *next = index
        .checked_add(1)
        .ok_or_else(|| io::Error::other("compact index overflow"))?;
    write_count(writer, delta)
}

impl ZcashSerialize for CompactBlock {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> io::Result<()> {
        self.validate().map_err(io::Error::other)?;
        self.header.zcash_serialize(&mut writer)?;
        writer.write_u64::<LittleEndian>(self.nonce)?;
        write_count(&mut writer, self.short_ids.len())?;
        for id in &self.short_ids {
            writer.write_all(id)?;
        }
        write_count(&mut writer, self.prefilled.len())?;
        let mut next = 0;
        for (index, tx) in &self.prefilled {
            write_index(&mut writer, *index, &mut next)?;
            tx.zcash_serialize(&mut writer)?;
        }
        Ok(())
    }
}

impl ZcashDeserialize for CompactBlock {
    fn zcash_deserialize<R: Read>(reader: R) -> Result<Self, SerializationError> {
        let mut reader = reader.take(u64::try_from(MAX_PROTOCOL_MESSAGE_LEN)?);
        let header = Arc::new(block::Header::zcash_deserialize(&mut reader)?);
        let nonce = reader.read_u64::<LittleEndian>()?;
        let count = read_count(&mut reader)?;
        let mut short_ids = vec_for_count(count);
        for _ in 0..count {
            let mut id = [0; 6];
            reader.read_exact(&mut id)?;
            short_ids.push(id);
        }
        let prefilled_count = read_count(&mut reader)?;
        let count = count
            .checked_add(prefilled_count)
            .ok_or(SerializationError::Parse(
                "compact transaction count overflow",
            ))?;
        check_count(count)?;
        let mut prefilled = vec_for_count(prefilled_count);
        let mut next = 0;
        let mut transaction_bytes = 0;
        for _ in 0..prefilled_count {
            let index = read_index(&mut reader, &mut next, count)?;
            let start = reader.limit();
            prefilled.push((
                index,
                Arc::new(Transaction::zcash_deserialize(&mut reader)?),
            ));
            transaction_bytes += start - reader.limit();
            check_transactions_size(prefilled.len(), transaction_bytes)?;
        }
        let compact = Self {
            header,
            nonce,
            short_ids,
            prefilled,
        };
        compact.validate()?;
        Ok(compact)
    }
}

impl ZcashSerialize for BlockTransactionsRequest {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> io::Result<()> {
        check_count(self.indexes.len()).map_err(io::Error::other)?;
        self.block_hash.zcash_serialize(&mut writer)?;
        write_count(&mut writer, self.indexes.len())?;
        let mut next = 0;
        for index in &self.indexes {
            check_count(index.saturating_add(1)).map_err(io::Error::other)?;
            write_index(&mut writer, *index, &mut next)?;
        }
        Ok(())
    }
}

impl ZcashDeserialize for BlockTransactionsRequest {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let block_hash = block::Hash::zcash_deserialize(&mut reader)?;
        let count = read_count(&mut reader)?;
        let mut indexes = vec_for_count(count);
        let mut next = 0;
        for _ in 0..count {
            indexes.push(read_index(
                &mut reader,
                &mut next,
                usize::try_from(Transaction::max_allocation())?,
            )?);
        }
        Ok(Self {
            block_hash,
            indexes,
        })
    }
}

impl ZcashSerialize for BlockTransactions {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> io::Result<()> {
        check_count(self.transactions.len()).map_err(io::Error::other)?;
        self.transactions
            .iter()
            .try_fold(0, |bytes, tx| add_bytes(bytes, tx.zcash_serialized_size()))
            .map_err(io::Error::other)?;
        self.block_hash.zcash_serialize(&mut writer)?;
        self.transactions.zcash_serialize(&mut writer)
    }
}

impl ZcashDeserialize for BlockTransactions {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let block_hash = block::Hash::zcash_deserialize(&mut reader)?;
        let count = read_count(&mut reader)?;
        let mut reader = reader.take(MAX_BLOCK_BYTES);
        let mut transactions = vec_for_count(count);
        for _ in 0..count {
            transactions.push(Arc::new(Transaction::zcash_deserialize(&mut reader)?));
            check_transactions_size(transactions.len(), MAX_BLOCK_BYTES - reader.limit())?;
        }
        Ok(Self {
            block_hash,
            transactions,
        })
    }
}
