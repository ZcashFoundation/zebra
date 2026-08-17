//! Request stream encodings for the version 2 Zcash P2P network protocol.
//!
//! A request stream is a bidirectional stream carrying exactly one request
//! and its response: the requester writes the stream type byte followed by
//! the request and finishes its sending direction, and the responder writes
//! the response and finishes its sending direction.

use std::io;

use tokio::io::AsyncRead;

use zebra_chain::block;

use super::{
    constants::{
        MAX_GET_BLOCKS_HASHES, MAX_GET_BLOCK_RANGE_BYTES, MAX_GET_BLOCK_RANGE_COUNT,
        MAX_GET_HASHES_COUNT, MAX_GET_OBJECT_LENGTH, MAX_GET_TREE_ROOTS_COUNT, MAX_GET_TX_REFS,
        MAX_LOCATOR_HASHES,
    },
    record,
    txref::TransactionReference,
    types::{ObjectHash, StreamType, WireError},
};

/// A request on a version 2 request stream.
//
// The variant names mirror the draft ZIP's request stream type names
// (`get-headers`, `get-blocks`, ...), so they intentionally share a prefix.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    /// Requests block headers starting after the first locator hash found in
    /// the responder's best chain, up to and including `stop` or 160
    /// headers, whichever comes first.
    GetHeaders {
        /// Block locator hashes, from highest to lowest height
        /// (at most [`MAX_LOCATOR_HASHES`]).
        known_blocks: Vec<block::Hash>,

        /// The hash of the last desired header, or `None` to request as many
        /// headers as possible.
        stop: Option<block::Hash>,

        /// Whether each returned header should be accompanied by the block's
        /// coinbase transaction and full transaction IDs, letting the
        /// requester fetch only the transactions it is missing via `get-tx`.
        tx_ids: bool,
    },

    /// Requests full blocks by hash
    /// (at most [`MAX_GET_BLOCKS_HASHES`] hashes).
    ///
    /// Compact blocks cannot be requested: they occur only as announcements.
    GetBlocks {
        /// The hashes of the requested blocks.
        hashes: Vec<block::Hash>,
    },

    /// Requests transactions by reference
    /// (at most [`MAX_GET_TX_REFS`] references).
    GetTx {
        /// The requested transaction references. All three reference types
        /// are permitted.
        refs: Vec<TransactionReference>,
    },

    /// Requests peer addresses. The request is empty.
    GetAddr,

    /// Requests references to the contents of the peer's transaction memory
    /// pool. The request is empty.
    GetMempool,

    /// Requests the hashes of the blocks at heights
    /// `start_height + k × stride` of the responder's best chain, for `k`
    /// from 0 to `count − 1`, with sync-cost metadata for each entry's span.
    GetHashes {
        /// The height of the first requested block hash.
        ///
        /// Wire heights are raw `u32` values: the whole range is encodable,
        /// and heights above the responder's chain tip are simply absent
        /// from the response.
        start_height: u32,

        /// The spacing between requested heights. Must not be 0.
        stride: u32,

        /// The maximum number of block hashes requested
        /// (at most [`MAX_GET_HASHES_COUNT`]). The greatest requested
        /// height must not exceed `u32::MAX`.
        count: u64,
    },

    /// Requests a contiguous chain of up to `count` blocks ending at
    /// `final_hash`, streamed in descending height order.
    GetBlockRange {
        /// The hash of the highest block in the requested range.
        final_hash: block::Hash,

        /// The maximum number of blocks requested
        /// (at most [`MAX_GET_BLOCK_RANGE_COUNT`]).
        count: u64,

        /// The maximum total serialized size of the delivered blocks, in
        /// bytes (at most [`MAX_GET_BLOCK_RANGE_BYTES`]). The responder
        /// delivers the first block regardless of this bound.
        max_bytes: u64,
    },

    /// Requests per-block note commitment tree roots, transaction counts,
    /// and authorizing data commitments for the blocks at heights
    /// `start_height` through `start_height + count − 1` of the chain
    /// identified by `final_hash`.
    GetTreeRoots {
        /// The height of the first requested entry.
        start_height: u32,

        /// The hash of the block at the highest requested height,
        /// `start_height + count − 1`, anchoring the request to a specific
        /// chain. A responder whose best chain does not contain this block
        /// at that height refuses the stream rather than serving entries
        /// for different blocks.
        final_hash: block::Hash,

        /// The maximum number of entries requested
        /// (at most [`MAX_GET_TREE_ROOTS_COUNT`]).
        count: u64,
    },

    /// Requests a byte range of a content-addressed synchronization
    /// artifact.
    GetObject {
        /// The SHA-256 hash of the requested object.
        hash: ObjectHash,

        /// The byte offset into the object at which to start.
        offset: u64,

        /// The maximum number of bytes requested
        /// (at most [`MAX_GET_OBJECT_LENGTH`]).
        length: u64,
    },
}

impl Request {
    /// Returns the stream type that carries this request.
    pub fn stream_type(&self) -> StreamType {
        match self {
            Request::GetHeaders { .. } => StreamType::GetHeaders,
            Request::GetBlocks { .. } => StreamType::GetBlocks,
            Request::GetTx { .. } => StreamType::GetTx,
            Request::GetAddr => StreamType::GetAddr,
            Request::GetMempool => StreamType::GetMempool,
            Request::GetHashes { .. } => StreamType::GetHashes,
            Request::GetBlockRange { .. } => StreamType::GetBlockRange,
            Request::GetTreeRoots { .. } => StreamType::GetTreeRoots,
            Request::GetObject { .. } => StreamType::GetObject,
        }
    }

    /// Encodes this request to `writer`, without the leading stream type
    /// byte.
    pub fn encode<W: io::Write>(&self, mut writer: W) -> Result<(), WireError> {
        match self {
            Request::GetHeaders {
                known_blocks,
                stop,
                tx_ids,
            } => {
                record::check_send_limit(known_blocks.len(), MAX_LOCATOR_HASHES, "locator hashes")?;

                record::write_compact_size(&mut writer, known_blocks.len() as u64)?;
                for hash in known_blocks {
                    writer.write_all(&hash.0)?;
                }
                writer.write_all(&stop.unwrap_or(block::Hash([0; 32])).0)?;
                writer.write_all(&[u8::from(*tx_ids)])?;
            }
            Request::GetBlocks { hashes } => {
                record::check_send_limit(hashes.len(), MAX_GET_BLOCKS_HASHES, "block requests")?;

                record::write_compact_size(&mut writer, hashes.len() as u64)?;
                for hash in hashes {
                    writer.write_all(&hash.0)?;
                }
            }
            Request::GetTx { refs } => {
                record::check_send_limit(refs.len(), MAX_GET_TX_REFS, "transaction references")?;

                record::write_compact_size(&mut writer, refs.len() as u64)?;
                for txref in refs {
                    txref.encode(&mut writer)?;
                }
            }
            Request::GetAddr | Request::GetMempool => {}
            Request::GetHashes {
                start_height,
                stride,
                count,
            } => {
                check_get_hashes_bounds(*start_height, *stride, *count)
                    .map_err(WireError::Local)?;

                writer.write_all(&start_height.to_le_bytes())?;
                writer.write_all(&stride.to_le_bytes())?;
                record::write_compact_size(&mut writer, *count)?;
            }
            Request::GetBlockRange {
                final_hash,
                count,
                max_bytes,
            } => {
                record::check_send_value(
                    *count,
                    MAX_GET_BLOCK_RANGE_COUNT as u64,
                    "get-block-range count",
                )?;
                record::check_send_value(
                    *max_bytes,
                    MAX_GET_BLOCK_RANGE_BYTES,
                    "get-block-range max_bytes",
                )?;

                writer.write_all(&final_hash.0)?;
                record::write_compact_size(&mut writer, *count)?;
                record::write_compact_size(&mut writer, *max_bytes)?;
            }
            Request::GetTreeRoots {
                start_height,
                final_hash,
                count,
            } => {
                record::check_send_value(
                    *count,
                    MAX_GET_TREE_ROOTS_COUNT as u64,
                    "get-tree-roots count",
                )?;

                writer.write_all(&start_height.to_le_bytes())?;
                writer.write_all(&final_hash.0)?;
                record::write_compact_size(&mut writer, *count)?;
            }
            Request::GetObject {
                hash,
                offset,
                length,
            } => {
                record::check_send_value(*length, MAX_GET_OBJECT_LENGTH, "get-object length")?;

                writer.write_all(&hash.0)?;
                record::write_compact_size(&mut writer, *offset)?;
                record::write_compact_size(&mut writer, *length)?;
            }
        }

        Ok(())
    }

    /// Reads a request of the given stream type from `reader`.
    ///
    /// The stream type byte has already been read to dispatch the stream.
    /// The caller checks that the stream ends after the request.
    ///
    /// # Panics
    ///
    /// If `stream_type` is not a request stream type.
    pub async fn read<R: AsyncRead + Unpin>(
        stream_type: StreamType,
        reader: &mut R,
    ) -> Result<Self, WireError> {
        match stream_type {
            StreamType::GetHeaders => {
                let locator_count = record::read_compact_size(reader).await?;
                record::check_recv_limit(locator_count, MAX_LOCATOR_HASHES, "locator")?;

                let mut known_blocks = Vec::with_capacity(record::preallocate_len(locator_count));
                for _ in 0..locator_count {
                    known_blocks.push(record::read_block_hash(reader).await?);
                }

                let stop = record::read_block_hash(reader).await?;
                let stop = (stop != block::Hash([0; 32])).then_some(stop);

                let tx_ids = match record::read_u8(reader).await? {
                    0 => false,
                    1 => true,
                    other => {
                        return Err(WireError::Protocol(format!(
                            "get-headers tx_ids must be 0 or 1, got {other:#04x}",
                        )))
                    }
                };

                Ok(Request::GetHeaders {
                    known_blocks,
                    stop,
                    tx_ids,
                })
            }
            StreamType::GetBlocks => {
                let count = record::read_compact_size(reader).await?;
                record::check_recv_limit(count, MAX_GET_BLOCKS_HASHES, "get-blocks")?;

                let mut hashes = Vec::with_capacity(record::preallocate_len(count));
                for _ in 0..count {
                    hashes.push(record::read_block_hash(reader).await?);
                }

                Ok(Request::GetBlocks { hashes })
            }
            StreamType::GetTx => {
                let count = record::read_compact_size(reader).await?;
                record::check_recv_limit(count, MAX_GET_TX_REFS, "get-tx")?;

                let mut refs = Vec::with_capacity(record::preallocate_len(count));
                for _ in 0..count {
                    refs.push(TransactionReference::read(reader).await?);
                }

                Ok(Request::GetTx { refs })
            }
            StreamType::GetAddr => Ok(Request::GetAddr),
            StreamType::GetMempool => Ok(Request::GetMempool),
            StreamType::GetHashes => {
                let start_height = record::read_exact_u32_le(reader).await?;
                let stride = record::read_exact_u32_le(reader).await?;
                let count = record::read_compact_size(reader).await?;

                check_get_hashes_bounds(start_height, stride, count)
                    .map_err(WireError::Protocol)?;

                Ok(Request::GetHashes {
                    start_height,
                    stride,
                    count,
                })
            }
            StreamType::GetBlockRange => {
                let final_hash = record::read_block_hash(reader).await?;
                let count = record::read_compact_size(reader).await?;
                record::check_recv_value(
                    count,
                    MAX_GET_BLOCK_RANGE_COUNT as u64,
                    "get-block-range count",
                )?;
                let max_bytes = record::read_compact_size(reader).await?;
                record::check_recv_value(
                    max_bytes,
                    MAX_GET_BLOCK_RANGE_BYTES,
                    "get-block-range max_bytes",
                )?;

                Ok(Request::GetBlockRange {
                    final_hash,
                    count,
                    max_bytes,
                })
            }
            StreamType::GetTreeRoots => {
                let start_height = record::read_exact_u32_le(reader).await?;
                let final_hash = record::read_block_hash(reader).await?;
                let count = record::read_compact_size(reader).await?;
                record::check_recv_value(
                    count,
                    MAX_GET_TREE_ROOTS_COUNT as u64,
                    "get-tree-roots count",
                )?;

                Ok(Request::GetTreeRoots {
                    start_height,
                    final_hash,
                    count,
                })
            }
            StreamType::GetObject => {
                let mut hash = [0u8; 32];
                record::read_exact_or_incomplete(reader, &mut hash).await?;
                let offset = record::read_compact_size(reader).await?;
                let length = record::read_compact_size(reader).await?;
                record::check_recv_value(length, MAX_GET_OBJECT_LENGTH, "get-object length")?;

                Ok(Request::GetObject {
                    hash: ObjectHash(hash),
                    offset,
                    length,
                })
            }
            StreamType::Handshake
            | StreamType::BlockAnnouncements
            | StreamType::TransactionAnnouncements
            | StreamType::AddressAnnouncements => {
                unreachable!("Request::read is only called for request stream types")
            }
        }
    }
}

/// Checks the bounds of a `get-hashes` request: `stride` must not be 0,
/// `count` must not exceed [`MAX_GET_HASHES_COUNT`], and the greatest
/// requested height (`start_height + (count − 1) × stride`) must not exceed
/// `u32::MAX`.
///
/// Returns a description of the violated bound: a local error when checked
/// by the encoder, and a protocol error when checked by the reader.
fn check_get_hashes_bounds(start_height: u32, stride: u32, count: u64) -> Result<(), String> {
    if stride == 0 {
        return Err("get-hashes stride must not be 0".to_string());
    }

    if count > MAX_GET_HASHES_COUNT as u64 {
        return Err(format!(
            "get-hashes count {count} exceeds the limit {MAX_GET_HASHES_COUNT}",
        ));
    }

    // The checks above bound this arithmetic well within `u64`: `count - 1`
    // is at most 49,999 and `stride` at most `u32::MAX`. A request for no
    // hashes has no greatest height to check.
    if let Some(last_index) = count.checked_sub(1) {
        let greatest_height = u64::from(start_height)
            .checked_add(last_index.saturating_mul(u64::from(stride)))
            .expect("bounded by 2^32 + 50,000 * 2^32, which fits in a u64");

        if greatest_height > u64::from(u32::MAX) {
            return Err(format!(
                "the greatest requested get-hashes height {greatest_height} exceeds the \
                 maximum encodable height",
            ));
        }
    }

    Ok(())
}
