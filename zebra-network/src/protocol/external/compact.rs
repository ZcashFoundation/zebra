//! Provisional compact relay encoding version 1, requiring bilateral early-relay consent.
//!
//! Short IDs commit to witnessed identities, not just transaction effects. Reconstruction checks
//! size, ordering, transaction Merkle roots and duplicates; callers must authenticate authorizing
//! data against the committed tip before submitting the resulting block to consensus. Hashing and
//! reconstruction are synchronous and must run on a blocking worker.

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
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
        CompactSizeMessage, SerializationError, TrustedPreallocate, ZcashDeserialize,
        ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN,
    },
    transaction::{Transaction, UnminedTx, UnminedTxId, MIN_TRANSPARENT_TX_SIZE},
};

#[cfg(test)]
mod tests;

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

/// Bounded partial reconstruction; not a consensus or authorizing-data validation verdict.
#[derive(Debug)]
pub struct Reconstruction {
    compact: Arc<CompactBlock>,
    transactions: Vec<Option<Arc<Transaction>>>,
}

impl CompactBlock {
    /// Builds an announcement with only the coinbase prefilled.
    ///
    /// The caller must use ordinary block relay if this encoding would not save bytes.
    pub fn from_block(block: &Block, nonce: u64) -> Result<Self, SerializationError> {
        check_count(block.transactions.len())?;
        check_bytes(block.zcash_serialized_size())?;
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

    /// Computes SipHash-2-4's low 48 bits over the legacy txid or full 64-byte witnessed identity.
    pub fn short_id(&self, id: UnminedTxId) -> [u8; 6] {
        short_id(self.keys(), id)
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
            if slot.is_none() {
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
        }
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
        let mut short_ids = Vec::with_capacity(count);
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
        let mut prefilled = Vec::with_capacity(prefilled_count);
        let mut next = 0;
        for _ in 0..prefilled_count {
            let index = read_index(&mut reader, &mut next, count)?;
            prefilled.push((
                index,
                Arc::new(Transaction::zcash_deserialize(&mut reader)?),
            ));
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
        let mut indexes = Vec::with_capacity(count);
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
        let mut transactions = Vec::with_capacity(count);
        for _ in 0..count {
            transactions.push(Arc::new(Transaction::zcash_deserialize(&mut reader)?));
        }
        Ok(Self {
            block_hash,
            transactions,
        })
    }
}
