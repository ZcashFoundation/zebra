use super::{config::*, error::*, *};

/// Zakura stream kind reserved for native block sync.
pub const ZAKURA_STREAM_BLOCK_SYNC: u16 = 6;
/// Capability bit for the native block-sync service.
pub const ZAKURA_CAP_BLOCK_SYNC: u64 = 1 << 3;
/// Version of the native block-sync stream.
pub const ZAKURA_BLOCK_SYNC_STREAM_VERSION: u16 = 1;

/// Peer status advertisement.
pub const MSG_BS_STATUS: u8 = 1;
/// Request a contiguous range of block bodies by height.
pub const MSG_BS_GET_BLOCKS: u8 = 2;
/// Respond with one full block body.
pub const MSG_BS_BLOCK: u8 = 3;
/// Terminate a `GetBlocks` response.
pub const MSG_BS_BLOCKS_DONE: u8 = 4;
/// Report that a requested range is not servable.
pub const MSG_BS_RANGE_UNAVAILABLE: u8 = 5;

/// Maximum block bodies ever requested or reported by stream 6.
pub const MAX_BS_BLOCKS_PER_REQUEST: u32 = 128;
/// Maximum encoded stream-6 message bytes.
///
/// This cap is intentionally larger than Zebra's consensus block-size limit so
/// stream-6 can read and classify slightly oversized or future-expanded frames
/// in the block-sync codec instead of dropping them at the raw transport gate.
/// Decoded `Block` messages are still bounded by [`block::MAX_BLOCK_BYTES`].
pub const MAX_BS_MESSAGE_BYTES: usize = 3 * 1024 * 1024;

pub(super) const BLOCK_SYNC_MESSAGE_TYPE_BYTES: usize = 1;

const _: () = assert!(MAX_BS_MESSAGE_BYTES < 4 * 1024 * 1024);
const _: () = assert!(MAX_BS_MESSAGE_BYTES > block::MAX_BLOCK_BYTES as usize);

/// Native stream-6 block-sync message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockSyncMessage {
    /// Servable range and serving capacity advertisement.
    Status(BlockSyncStatus),
    /// Request `count` block bodies starting at `start_height`.
    GetBlocks {
        /// First requested height.
        start_height: block::Height,
        /// Requested block count.
        count: u32,
    },
    /// One full block body.
    Block(Arc<block::Block>),
    /// End of a `GetBlocks` response.
    BlocksDone {
        /// First requested height.
        start_height: block::Height,
        /// Number of blocks returned.
        returned: u32,
    },
    /// The peer cannot serve this range.
    RangeUnavailable {
        /// First unavailable height.
        start_height: block::Height,
        /// Unavailable block count.
        count: u32,
    },
}

impl BlockSyncMessage {
    /// Returns this message's stream-6 discriminator.
    pub fn message_type(&self) -> u8 {
        match self {
            Self::Status(_) => MSG_BS_STATUS,
            Self::GetBlocks { .. } => MSG_BS_GET_BLOCKS,
            Self::Block(_) => MSG_BS_BLOCK,
            Self::BlocksDone { .. } => MSG_BS_BLOCKS_DONE,
            Self::RangeUnavailable { .. } => MSG_BS_RANGE_UNAVAILABLE,
        }
    }

    /// Encode this message as `[u8 message_type][bounded fields...]`.
    pub fn encode(&self) -> Result<Vec<u8>, BlockSyncWireError> {
        let mut bytes = Vec::new();
        bytes.write_u8(self.message_type())?;
        match self {
            Self::Status(status) => status.encode_to(&mut bytes)?,
            Self::GetBlocks {
                start_height,
                count,
            }
            | Self::RangeUnavailable {
                start_height,
                count,
            } => {
                validate_block_count(*count)?;
                write_height(&mut bytes, *start_height)?;
                bytes.write_u32::<LittleEndian>(*count)?;
            }
            Self::Block(block) => {
                block.zcash_serialize(&mut bytes)?;
                validate_encoded_block_len(
                    bytes.len().saturating_sub(BLOCK_SYNC_MESSAGE_TYPE_BYTES),
                )?;
            }
            Self::BlocksDone {
                start_height,
                returned,
            } => {
                validate_block_count(*returned)?;
                write_height(&mut bytes, *start_height)?;
                bytes.write_u32::<LittleEndian>(*returned)?;
            }
        }
        validate_payload_len(bytes.len())?;
        Ok(bytes)
    }

    /// Decode a stream-6 message.
    pub fn decode(bytes: &[u8]) -> Result<Self, BlockSyncWireError> {
        validate_payload_len(bytes.len())?;
        let mut reader = Cursor::new(bytes);
        let message_type = reader.read_u8()?;
        let message = match message_type {
            MSG_BS_STATUS => Self::Status(BlockSyncStatus::decode_from(&mut reader)?),
            MSG_BS_GET_BLOCKS => {
                let start_height = read_height(&mut reader)?;
                let count = reader.read_u32::<LittleEndian>()?;
                validate_block_count(count)?;
                Self::GetBlocks {
                    start_height,
                    count,
                }
            }
            MSG_BS_BLOCK => {
                let block_start = usize::try_from(reader.position())
                    .map_err(|_| BlockSyncWireError::NumericOverflow("block payload offset"))?;
                let block = Arc::new(block::Block::zcash_deserialize(&mut reader)?);
                let block_end = usize::try_from(reader.position())
                    .map_err(|_| BlockSyncWireError::NumericOverflow("block payload end"))?;
                validate_encoded_block_len(block_end.saturating_sub(block_start))?;
                Self::Block(block)
            }
            MSG_BS_BLOCKS_DONE => {
                let start_height = read_height(&mut reader)?;
                let returned = reader.read_u32::<LittleEndian>()?;
                validate_block_count(returned)?;
                Self::BlocksDone {
                    start_height,
                    returned,
                }
            }
            MSG_BS_RANGE_UNAVAILABLE => {
                let start_height = read_height(&mut reader)?;
                let count = reader.read_u32::<LittleEndian>()?;
                validate_block_count(count)?;
                Self::RangeUnavailable {
                    start_height,
                    count,
                }
            }
            value => return Err(BlockSyncWireError::UnknownMessageType(value)),
        };
        reject_trailing(bytes, &reader)?;
        Ok(message)
    }

    /// Convert this message into a bounded Zakura frame.
    pub fn encode_frame(&self) -> Result<Frame, BlockSyncWireError> {
        Ok(Frame {
            message_type: u16::from(self.message_type()),
            flags: 0,
            payload: self.encode()?,
        })
    }

    /// Decode this message from a Zakura frame after checking flags and type agreement.
    pub fn decode_frame(frame: Frame) -> Result<Self, BlockSyncWireError> {
        if frame.flags != 0 {
            return Err(BlockSyncWireError::UnsupportedFlags(frame.flags));
        }
        let message = Self::decode(&frame.payload)?;
        let frame_message_type = u8::try_from(frame.message_type)
            .map_err(|_| BlockSyncWireError::UnknownFrameMessageType(frame.message_type))?;
        if frame_message_type != message.message_type() {
            return Err(BlockSyncWireError::MismatchedFrameMessageType {
                frame: frame.message_type,
                payload: message.message_type(),
            });
        }
        Ok(message)
    }
}

pub(super) fn validate_block_count(count: u32) -> Result<(), BlockSyncWireError> {
    if count == 0 {
        return Err(BlockSyncWireError::ZeroBlockCount);
    }
    if count > MAX_BS_BLOCKS_PER_REQUEST {
        return Err(BlockSyncWireError::BlockCountLimit {
            actual: count,
            max: MAX_BS_BLOCKS_PER_REQUEST,
        });
    }
    Ok(())
}

pub(super) fn validate_payload_len(len: usize) -> Result<(), BlockSyncWireError> {
    if len > MAX_BS_MESSAGE_BYTES {
        return Err(BlockSyncWireError::OversizedPayload {
            actual: len,
            max: MAX_BS_MESSAGE_BYTES,
        });
    }
    Ok(())
}

fn validate_encoded_block_len(len: usize) -> Result<(), BlockSyncWireError> {
    let max = usize::try_from(block::MAX_BLOCK_BYTES)
        .map_err(|_| BlockSyncWireError::NumericOverflow("max block bytes"))?;
    if len > max {
        return Err(BlockSyncWireError::OversizedBlock { actual: len, max });
    }
    Ok(())
}

pub(super) fn write_height<W: Write>(
    writer: &mut W,
    height: block::Height,
) -> Result<(), BlockSyncWireError> {
    writer.write_u32::<LittleEndian>(height.0)?;
    Ok(())
}

pub(super) fn read_height<R: Read>(reader: &mut R) -> Result<block::Height, BlockSyncWireError> {
    let raw = reader.read_u32::<LittleEndian>()?;
    block::Height::try_from(raw).map_err(|_| BlockSyncWireError::HeightOutOfRange(raw))
}

pub(super) fn reject_trailing(
    bytes: &[u8],
    reader: &Cursor<&[u8]>,
) -> Result<(), BlockSyncWireError> {
    let consumed = usize::try_from(reader.position())
        .map_err(|_| BlockSyncWireError::NumericOverflow("payload cursor"))?;
    if consumed != bytes.len() {
        return Err(BlockSyncWireError::TrailingBytes);
    }
    Ok(())
}
