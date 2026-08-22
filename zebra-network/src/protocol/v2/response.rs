//! Response encodings for the version 2 Zcash P2P network protocol request
//! streams.
//!
//! `get-blocks` and `get-tx` responses carry one result-tagged entry per
//! requested item, in request order, so their entries are encoded and read
//! individually. The other responses are single structures.

use std::{io, sync::Arc};

use tokio::io::AsyncRead;

use zebra_chain::{
    block::{self, Block, CountedHeader, Header, SyncHashEntry, TreeRootsEntry},
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::Transaction,
};

use super::{
    constants::{
        MAX_ADDRS_IN_RESPONSE, MAX_GET_HASHES_COUNT, MAX_GET_TREE_ROOTS_COUNT, MAX_HEADERS_RESULTS,
        MAX_MEMPOOL_RESPONSE_REFS, MAX_RECORD_PAYLOAD_LEN, MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED,
    },
    record,
    txref::TransactionReference,
    types::WireError,
};

use crate::{
    meta_addr::MetaAddr,
    protocol::external::addr::v2::{AddrV2, MAX_ADDR_V2_ADDR_SIZE},
};

/// The result byte indicating that a full object follows.
pub const RESULT_OBJECT: u8 = 0x00;

/// The result byte indicating that the requested item was not found.
/// Nothing follows for the entry.
pub const RESULT_NOT_FOUND: u8 = 0x02;

/// A `get-headers` response: up to [`MAX_HEADERS_RESULTS`] block headers,
/// each length-prefixed.
///
/// The legacy always-zero transaction count that followed each header in
/// `headers` messages is removed; [`CountedHeader`] is used here only
/// because it is the header type of the internal protocol.
#[derive(Clone, Debug)]
pub struct HeadersResponse(pub Vec<CountedHeader>);

impl HeadersResponse {
    /// Encodes this response to `writer`.
    #[allow(clippy::unwrap_in_result)]
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        record::write_compact_size(&mut writer, self.0.len() as u64)?;

        for header in &self.0 {
            let header_bytes = header
                .header
                .zcash_serialize_to_vec()
                .expect("serializing a header to a Vec never fails");
            record::write_record(&mut writer, &header_bytes)?;
        }

        Ok(())
    }

    /// Reads a `get-headers` response from `reader`.
    ///
    /// A response with more than [`MAX_HEADERS_RESULTS`] headers incurs a
    /// misbehavior penalty instead of being read.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let count = record::read_compact_size(reader).await?;
        record::check_recv_limit_scored(count, MAX_HEADERS_RESULTS, "get-headers")?;

        let mut headers = Vec::with_capacity(record::preallocate_len(count));
        for _ in 0..count {
            let header_bytes =
                record::read_length_prefixed_bytes(reader, MAX_RECORD_PAYLOAD_LEN).await?;
            let header = Arc::new(Header::zcash_deserialize(header_bytes.as_slice())?);
            headers.push(CountedHeader { header });
        }

        Ok(HeadersResponse(headers))
    }

    /// Checks that the headers form a contiguous chain: each header's
    /// previous block hash must match the hash of the preceding header.
    /// Returns the hash of each header, in response order.
    ///
    /// Non-contiguous headers incur a misbehavior penalty.
    pub fn check_contiguous(&self) -> Result<Vec<block::Hash>, WireError> {
        let mut hashes: Vec<block::Hash> = Vec::with_capacity(self.0.len());

        for header in &self.0 {
            if let Some(previous) = hashes.last() {
                if header.header.previous_block_hash != *previous {
                    return Err(WireError::Misbehavior {
                        points: MISBEHAVIOR_PENALTY_LIMIT_EXCEEDED,
                        reason: "non-contiguous headers in a get-headers response".to_string(),
                    });
                }
            }
            hashes.push(header.header.hash());
        }

        Ok(hashes)
    }
}

/// One entry of a `get-blocks` response.
#[derive(Clone, Debug)]
pub enum BlockResponseEntry {
    /// A full block.
    Full(Arc<Block>),

    /// The block was not found.
    NotFound,
}

impl BlockResponseEntry {
    /// Encodes this entry to `writer`.
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        match self {
            BlockResponseEntry::Full(block) => {
                writer.write_all(&[RESULT_OBJECT])?;
                let block_bytes = block
                    .zcash_serialize_to_vec()
                    .map_err(|err| WireError::Local(format!("unserializable block: {err}")))?;
                record::write_record(&mut writer, &block_bytes)?;
            }
            BlockResponseEntry::NotFound => {
                writer.write_all(&[RESULT_NOT_FOUND])?;
            }
        }

        Ok(())
    }

    /// Reads one entry of a `get-blocks` response from `reader`.
    ///
    /// An unrecognized `result` value is a connection error of type
    /// `PROTOCOL_ERROR`. Compact blocks occur only as announcements, so
    /// there is no compact block result.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        match record::read_u8(reader).await? {
            RESULT_OBJECT => {
                let block_bytes =
                    record::read_length_prefixed_bytes(reader, MAX_RECORD_PAYLOAD_LEN).await?;
                let block = Arc::new(Block::zcash_deserialize(block_bytes.as_slice())?);
                Ok(BlockResponseEntry::Full(block))
            }
            RESULT_NOT_FOUND => Ok(BlockResponseEntry::NotFound),
            unknown => Err(WireError::Protocol(format!(
                "unrecognized get-blocks result value {unknown:#04x}",
            ))),
        }
    }
}

/// One entry of a `get-tx` response.
#[derive(Clone, Debug)]
pub enum TxResponseEntry {
    /// The requested transaction.
    Found(Arc<Transaction>),

    /// The transaction was not found.
    NotFound,
}

impl TxResponseEntry {
    /// Encodes this entry to `writer`.
    #[allow(clippy::unwrap_in_result)]
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        match self {
            TxResponseEntry::Found(tx) => {
                writer.write_all(&[RESULT_OBJECT])?;
                let tx_bytes = tx
                    .zcash_serialize_to_vec()
                    .expect("serializing a transaction to a Vec never fails");
                record::write_record(&mut writer, &tx_bytes)?;
            }
            TxResponseEntry::NotFound => {
                writer.write_all(&[RESULT_NOT_FOUND])?;
            }
        }

        Ok(())
    }

    /// Reads one entry of a `get-tx` response from `reader`.
    ///
    /// An unrecognized `result` value is a connection error of type
    /// `PROTOCOL_ERROR`. A compact block result is only defined for
    /// `get-blocks` responses, so it is unrecognized here.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        match record::read_u8(reader).await? {
            RESULT_OBJECT => {
                let tx_bytes =
                    record::read_length_prefixed_bytes(reader, MAX_RECORD_PAYLOAD_LEN).await?;
                let tx = Arc::new(Transaction::zcash_deserialize(tx_bytes.as_slice())?);
                Ok(TxResponseEntry::Found(tx))
            }
            RESULT_NOT_FOUND => Ok(TxResponseEntry::NotFound),
            unknown => Err(WireError::Protocol(format!(
                "unrecognized get-tx result value {unknown:#04x}",
            ))),
        }
    }
}

/// A `get-addr` response: up to [`MAX_ADDRS_IN_RESPONSE`] network address
/// records in the `addrv2` encoding of ZIP 155.
#[derive(Clone, Debug)]
pub struct AddrResponse(pub Vec<MetaAddr>);

impl AddrResponse {
    /// Encodes this response to `writer`.
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        record::check_send_limit(self.0.len(), MAX_ADDRS_IN_RESPONSE, "address records")?;

        record::write_compact_size(&mut writer, self.0.len() as u64)?;
        for addr in &self.0 {
            AddrV2::from(addr.clone()).zcash_serialize(&mut writer)?;
        }

        Ok(())
    }

    /// Reads a `get-addr` response from `reader`.
    ///
    /// A response with more than [`MAX_ADDRS_IN_RESPONSE`] records incurs a
    /// misbehavior penalty instead of being read. Address records with
    /// unrecognized network IDs are ignored.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let count = record::read_compact_size(reader).await?;
        record::check_recv_limit_scored(count, MAX_ADDRS_IN_RESPONSE, "get-addr")?;

        let mut addrs = Vec::with_capacity(record::preallocate_len(count));
        for _ in 0..count {
            let addr = read_addr_v2(reader).await?;
            if let Ok(addr) = MetaAddr::try_from(addr) {
                addrs.push(addr);
            }
        }

        Ok(AddrResponse(addrs))
    }
}

/// A `get-mempool` response: references to the contents of the peer's
/// transaction memory pool.
#[derive(Clone, Debug)]
pub struct MempoolResponse(pub Vec<TransactionReference>);

impl MempoolResponse {
    /// Encodes this response to `writer`.
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        record::write_compact_size(&mut writer, self.0.len() as u64)?;
        for txref in &self.0 {
            assert!(
                !txref.is_short_id(),
                "SHORTID references must not be used in get-mempool responses",
            );
            txref.encode(&mut writer)?;
        }

        Ok(())
    }

    /// Reads a `get-mempool` response from `reader`.
    ///
    /// `SHORTID` references are a connection error of type `PROTOCOL_ERROR`:
    /// they must not be used in `get-mempool` responses.
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let count = record::read_compact_size(reader).await?;
        if count > MAX_MEMPOOL_RESPONSE_REFS as u64 {
            return Err(WireError::Flood(format!(
                "get-mempool response count {count} exceeds the limit \
                 {MAX_MEMPOOL_RESPONSE_REFS}",
            )));
        }

        let mut refs = Vec::with_capacity(record::preallocate_len(count));
        for _ in 0..count {
            let txref = TransactionReference::read(reader).await?;
            if txref.is_short_id() {
                return Err(WireError::Protocol(
                    "SHORTID reference in a get-mempool response".to_string(),
                ));
            }
            refs.push(txref);
        }

        Ok(MempoolResponse(refs))
    }
}

/// A `get-hashes` response: up to the requested number of entries, one per
/// requested height, truncated to a prefix.
#[derive(Clone, Debug)]
pub struct HashesResponse(pub Vec<SyncHashEntry>);

impl HashesResponse {
    /// Encodes this response to `writer`.
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        record::write_compact_size(&mut writer, self.0.len() as u64)?;
        for entry in &self.0 {
            writer.write_all(&entry.hash.0)?;
            record::write_compact_size(&mut writer, entry.span_size)?;
            record::write_compact_size(&mut writer, entry.span_txs)?;
            record::write_compact_size(&mut writer, entry.span_notes)?;
        }

        Ok(())
    }

    /// Reads a `get-hashes` response from `reader`.
    //
    // Used by round-trip tests; the v2 synchronization requester (Phase 5)
    // will use it outside tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let count = record::read_compact_size(reader).await?;
        record::check_recv_limit(count, MAX_GET_HASHES_COUNT, "get-hashes response")?;

        let mut entries = Vec::with_capacity(record::preallocate_len(count));
        for _ in 0..count {
            entries.push(SyncHashEntry {
                hash: record::read_block_hash(reader).await?,
                span_size: record::read_compact_size(reader).await?,
                span_txs: record::read_compact_size(reader).await?,
                span_notes: record::read_compact_size(reader).await?,
            });
        }

        Ok(HashesResponse(entries))
    }
}

/// A `get-tree-roots` response: up to the requested number of entries, one
/// per requested height, truncated to a prefix.
#[derive(Clone, Debug)]
pub struct TreeRootsResponse(pub Vec<TreeRootsEntry>);

impl TreeRootsResponse {
    /// Encodes this response to `writer`.
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        record::write_compact_size(&mut writer, self.0.len() as u64)?;
        for entry in &self.0 {
            writer.write_all(&entry.sapling_root)?;
            writer.write_all(&entry.orchard_root)?;
            writer.write_all(&entry.ironwood_root)?;
            record::write_compact_size(&mut writer, entry.sapling_txs)?;
            record::write_compact_size(&mut writer, entry.orchard_txs)?;
            record::write_compact_size(&mut writer, entry.ironwood_txs)?;
            writer.write_all(&entry.auth_data_root)?;
        }

        Ok(())
    }

    /// Reads a `get-tree-roots` response from `reader`.
    //
    // Used by round-trip tests; the v2 synchronization requester (Phase 5)
    // will use it outside tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, WireError> {
        let count = record::read_compact_size(reader).await?;
        record::check_recv_limit(count, MAX_GET_TREE_ROOTS_COUNT, "get-tree-roots response")?;

        let mut entries = Vec::with_capacity(record::preallocate_len(count));
        for _ in 0..count {
            entries.push(TreeRootsEntry {
                sapling_root: record::read_array(reader).await?,
                orchard_root: record::read_array(reader).await?,
                ironwood_root: record::read_array(reader).await?,
                sapling_txs: record::read_compact_size(reader).await?,
                orchard_txs: record::read_compact_size(reader).await?,
                ironwood_txs: record::read_compact_size(reader).await?,
                auth_data_root: record::read_array(reader).await?,
            });
        }

        Ok(TreeRootsResponse(entries))
    }
}

/// Reads a single `addrv2` network address record from `reader`.
///
/// The `addrv2` encoding is variable-length, so the record is deserialized
/// incrementally: the fixed-size prefix through the `sizeAddr` length, then
/// the address bytes and port.
async fn read_addr_v2<R: AsyncRead + Unpin>(reader: &mut R) -> Result<AddrV2, WireError> {
    // time (4) || services (CompactSize) || networkID (1)
    let mut time = [0u8; 4];
    record::read_exact_or_incomplete(reader, &mut time).await?;
    let services = record::read_compact_size(reader).await?;
    let network_id = record::read_u8(reader).await?;

    // sizeAddr (CompactSize) || addr || port (2)
    let addr_bytes = record::read_length_prefixed_bytes(reader, MAX_ADDR_V2_ADDR_SIZE).await?;
    let mut port = [0u8; 2];
    record::read_exact_or_incomplete(reader, &mut port).await?;

    // Re-encode the complete record and reuse the strict `addrv2`
    // deserializer, so length and network ID validation stay in one place.
    let mut buf = Vec::with_capacity(16 + addr_bytes.len());
    buf.extend_from_slice(&time);
    record::write_compact_size(&mut buf, services)?;
    buf.push(network_id);
    record::write_compact_size(&mut buf, addr_bytes.len() as u64)?;
    buf.extend_from_slice(&addr_bytes);
    buf.extend_from_slice(&port);

    Ok(AddrV2::zcash_deserialize(buf.as_slice())?)
}
