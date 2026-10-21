//! Requests (draft, "Request Stream Types"): the bytes after a request
//! stream's type byte up to the requester's FIN, decoded from the complete
//! body, which the stream's owner reads up to [`max_body_len`].

use std::io::Write;

use bytes::Bytes;

use zebra_chain::{
    block,
    serialization::{ZcashDeserialize, ZcashSerialize},
};

use super::{
    constants::{MAX_BLOCKS_PER_REQUEST, MAX_LOCATOR_HASHES, MAX_TXS_PER_REQUEST},
    txref::TxRef,
    types::StreamType,
    wire::{ensure_consumed, read_count, read_flag, write_count, WireError},
};

/// The longest body a conforming request on `stream_type` can have, or `None`
/// for a type this node does not serve; a request is read up to this bound, so
/// a peer cannot make this node buffer more than its format allows.
pub fn max_body_len(stream_type: StreamType) -> Option<usize> {
    Some(match stream_type {
        // A 3-byte count, the locator hashes, the stop hash, and the flag.
        StreamType::GetHeaders => 3 + MAX_LOCATOR_HASHES * 32 + 32 + 1,
        // A 3-byte count and the hashes.
        StreamType::GetBlocks => 3 + MAX_BLOCKS_PER_REQUEST * 32,
        // A 3-byte count and references of the largest type.
        StreamType::GetTx => 3 + MAX_TXS_PER_REQUEST * TxRef::MAX_ENCODED_LEN,
        StreamType::GetAddr | StreamType::GetMempool => 0,
        _ => return None,
    })
}

/// A request on one of the request stream types this node serves.
///
/// Variants are named for the draft's stream types without the `get-`
/// prefix: `Request::Headers` is a `get-headers` request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// A `get-headers` request (stream type `0x01`).
    Headers(GetHeaders),

    /// A `get-blocks` request (stream type `0x02`).
    Blocks(GetBlocks),

    /// A `get-tx` request (stream type `0x03`).
    Tx(GetTx),

    /// A `get-addr` request (stream type `0x04`); its body is empty.
    Addr,

    /// A `get-mempool` request (stream type `0x05`); its body is empty and its
    /// response is an open-ended sequence of transaction reference records.
    Mempool,
}

impl Request {
    /// The stream type this request is sent on.
    pub fn stream_type(&self) -> StreamType {
        match self {
            Request::Headers(_) => StreamType::GetHeaders,
            Request::Blocks(_) => StreamType::GetBlocks,
            Request::Tx(_) => StreamType::GetTx,
            Request::Addr => StreamType::GetAddr,
            Request::Mempool => StreamType::GetMempool,
        }
    }

    /// Encodes the request body, without the stream type byte.
    pub fn encode(&self) -> Result<Bytes, WireError> {
        let mut body = Vec::new();
        match self {
            Request::Headers(request) => request.write(&mut body)?,
            Request::Blocks(request) => request.write(&mut body)?,
            Request::Tx(request) => request.write(&mut body)?,
            Request::Addr | Request::Mempool => {}
        }
        Ok(body.into())
    }

    /// Decodes the complete body of a request received on `stream_type`.
    ///
    /// Only the five served request types have a format here; any other type
    /// is [`NotARequest`](WireError::NotARequest). The stream's owner decides
    /// before calling whether such a stream is refused or a connection error,
    /// and refuses unknown type bytes as `UNSUPPORTED_STREAM_TYPE` without
    /// reading a request.
    pub fn decode(stream_type: StreamType, body: &[u8]) -> Result<Self, WireError> {
        let mut reader = body;
        let request = match stream_type {
            StreamType::GetHeaders => Request::Headers(GetHeaders::read(&mut reader)?),
            StreamType::GetBlocks => Request::Blocks(GetBlocks::read(&mut reader)?),
            StreamType::GetTx => Request::Tx(GetTx::read(&mut reader)?),
            StreamType::GetAddr => Request::Addr,
            StreamType::GetMempool => Request::Mempool,
            stream_type => return Err(WireError::NotARequest { stream_type }),
        };
        ensure_consumed(reader, "request")?;
        Ok(request)
    }
}

/// A `get-headers` request (draft, "`get-headers`").
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetHeaders {
    /// Block locator hashes, from highest to lowest height; at most
    /// [`MAX_LOCATOR_HASHES`].
    ///
    /// Headers start after the first of these found in the responder's best
    /// chain. If empty, the response is the single header `hash_stop` names.
    pub locator_hashes: Vec<block::Hash>,

    /// The hash of the last desired header, or all zeros for as many as
    /// possible.
    pub hash_stop: block::Hash,

    /// Whether each returned header should carry the block's coinbase
    /// transaction and transaction IDs.
    pub tx_ids: bool,
}

impl GetHeaders {
    /// Writes the request body.
    fn write<W: Write>(&self, mut writer: W) -> Result<(), WireError> {
        write_count(
            &mut writer,
            "locator_count",
            self.locator_hashes.len(),
            MAX_LOCATOR_HASHES,
        )?;
        for hash in &self.locator_hashes {
            hash.zcash_serialize(&mut writer)?;
        }
        self.hash_stop.zcash_serialize(&mut writer)?;
        writer.write_all(&[u8::from(self.tx_ids)])?;
        Ok(())
    }

    fn read(reader: &mut &[u8]) -> Result<Self, WireError> {
        let locator_count =
            read_count(reader, "locator_count", MAX_LOCATOR_HASHES, count_too_large)?;
        let locator_hashes = read_hashes(reader, locator_count, "locator_hashes")?;
        let hash_stop = block::Hash::zcash_deserialize(&mut *reader)
            .map_err(WireError::malformed("hash_stop"))?;
        let tx_ids = read_flag(reader, "tx_ids")?;

        Ok(GetHeaders {
            locator_hashes,
            hash_stop,
            tx_ids,
        })
    }
}

/// A `get-blocks` request (draft, "`get-blocks`").
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetBlocks {
    /// The hashes of the blocks to fetch; at most [`MAX_BLOCKS_PER_REQUEST`].
    pub hashes: Vec<block::Hash>,
}

impl GetBlocks {
    /// Writes the request body.
    fn write<W: Write>(&self, mut writer: W) -> Result<(), WireError> {
        write_count(
            &mut writer,
            "count",
            self.hashes.len(),
            MAX_BLOCKS_PER_REQUEST,
        )?;
        for hash in &self.hashes {
            hash.zcash_serialize(&mut writer)?;
        }
        Ok(())
    }

    fn read(reader: &mut &[u8]) -> Result<Self, WireError> {
        let count = read_count(reader, "count", MAX_BLOCKS_PER_REQUEST, count_too_large)?;
        let hashes = read_hashes(reader, count, "hashes")?;
        Ok(GetBlocks { hashes })
    }
}

/// A `get-tx` request (draft, "`get-tx`").
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetTx {
    /// The references of the transactions to fetch; at most
    /// [`MAX_TXS_PER_REQUEST`].
    pub txrefs: Vec<TxRef>,
}

impl GetTx {
    /// Writes the request body.
    fn write<W: Write>(&self, mut writer: W) -> Result<(), WireError> {
        write_count(&mut writer, "count", self.txrefs.len(), MAX_TXS_PER_REQUEST)?;
        for txref in &self.txrefs {
            txref.write(&mut writer)?;
        }
        Ok(())
    }

    fn read(reader: &mut &[u8]) -> Result<Self, WireError> {
        let count = read_count(reader, "count", MAX_TXS_PER_REQUEST, count_too_large)?;

        // Grown as references arrive, not reserved from the count, so a short body claiming
        // the maximum count allocates nothing.
        let mut txrefs = Vec::new();
        for _ in 0..count {
            txrefs.push(TxRef::read(reader)?);
        }

        Ok(GetTx { txrefs })
    }
}

/// The request-side count violation: a connection error of type
/// `PROTOCOL_ERROR`.
fn count_too_large(field: &'static str, count: u64, max: usize) -> WireError {
    WireError::CountTooLarge { field, count, max }
}

/// Reads `count` block hashes from `reader`.
///
/// `count` has already been checked against its bound, and the hashes are
/// pushed as they arrive rather than reserved from the count, so the
/// allocation stays proportional to the bytes actually present.
fn read_hashes(
    reader: &mut &[u8],
    count: usize,
    what: &'static str,
) -> Result<Vec<block::Hash>, WireError> {
    let mut hashes = Vec::new();
    for _ in 0..count {
        hashes.push(
            block::Hash::zcash_deserialize(&mut *reader).map_err(WireError::malformed(what))?,
        );
    }
    Ok(hashes)
}
