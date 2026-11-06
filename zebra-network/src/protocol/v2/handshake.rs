//! Handshake stream records: the `init` record and its kind byte (draft,
//! "Connection Handshake", "Init Record", "Handshake Validation").
//!
//! Nonce and version rules are the handshake's; the single-`init` rule is
//! the connection's.

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;

use zebra_chain::{
    block,
    serialization::{
        zcash_deserialize_string_external_count, CompactSize64, CompactSizeMessage,
        SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
    },
};

use crate::protocol::external::{
    types::{Nonce, PeerServices, Version},
    MAX_USER_AGENT_LENGTH,
};

/// The record kind byte of an `init` record (draft, "Connection Handshake").
pub const INIT_RECORD_KIND: u8 = 0x00;

/// Decodes a complete handshake stream record payload, as the record codec
/// yields it: `Some` for an `init` record, `None` for a record kind the
/// draft says to ignore ("Connection Handshake").
///
/// Trailing bytes after an `init` record are an error: every field is
/// mandatory and new handshake data comes under new record kinds.
pub fn decode_record(payload: &[u8]) -> Result<Option<InitRecord>, SerializationError> {
    let mut reader = payload;

    if reader.read_u8()? != INIT_RECORD_KIND {
        return Ok(None);
    }

    let init = InitRecord::zcash_deserialize(&mut reader)?;

    if !reader.is_empty() {
        return Err(SerializationError::Parse(
            "trailing bytes after init record",
        ));
    }

    Ok(Some(init))
}

/// The `init` record (draft, "Init Record").
///
/// Field order and widths are the wire layout. Unlike the legacy `version`
/// message there is no timestamp and no address pair, and the three trailing
/// flags are mandatory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitRecord {
    /// The sender's advertised protocol version (draft, "Protocol Versioning").
    pub version: Version,

    /// The sender's service flags; unknown bits are kept, not truncated, and
    /// ignored (draft, "Service Flags"). They are not relayed: Zebra does not
    /// add a version 2 peer's own services to the address book, and address
    /// records discard unknown bits when they are read.
    pub services: PeerServices,

    /// A random nonce for self-connection detection.
    pub nonce: Nonce,

    /// The sender's user agent, at most `MAX_USER_AGENT_LENGTH` (256) bytes.
    pub user_agent: String,

    /// The best block height known to the sender.
    pub start_height: block::Height,

    /// Whether the sender wants transaction relay: if false, this node must
    /// not open a transaction announcement stream to the sender (draft, "Init
    /// Record").
    pub relay: bool,

    /// Whether the sender requests high-bandwidth compact block
    /// announcements (draft, "Compact Block Relay").
    pub announce: bool,

    /// Whether the sender requests full transaction IDs in compact block
    /// announcements (draft, "Full Transaction IDs").
    pub full_ids: bool,
}

impl InitRecord {
    /// Encodes this record as a complete handshake stream record payload,
    /// kind byte first, ready for the [record codec](super::record::RecordCodec).
    pub(crate) fn encode(&self) -> Bytes {
        let mut payload = vec![INIT_RECORD_KIND];
        self.zcash_serialize(&mut payload).expect(
            "serializing into a Vec cannot fail: its Write impl is infallible, and the user agent \
             is at most 256 bytes, asserted in `peer_set::initialize::v2::start`, so every field \
             encodes",
        );
        payload.into()
    }
}

impl ZcashSerialize for InitRecord {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.version.0)?;
        CompactSize64::from(self.services.bits()).zcash_serialize(&mut writer)?;
        writer.write_u64::<LittleEndian>(self.nonce.0)?;
        self.user_agent.zcash_serialize(&mut writer)?;
        writer.write_u32::<LittleEndian>(self.start_height.0)?;
        writer.write_u8(self.relay.into())?;
        writer.write_u8(self.announce.into())?;
        writer.write_u8(self.full_ids.into())?;

        Ok(())
    }
}

impl ZcashDeserialize for InitRecord {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let version = Version(reader.read_u32::<LittleEndian>()?);

        let services: CompactSize64 = (&mut reader).zcash_deserialize_into()?;
        let services = PeerServices::from_bits_retain(services.into());

        let nonce = Nonce(reader.read_u64::<LittleEndian>()?);

        // As the legacy version message reader: the length is checked before the bytes are read,
        // so an oversized claim allocates nothing (draft, "Handshake Validation").
        let byte_count: CompactSizeMessage = (&mut reader).zcash_deserialize_into()?;
        let byte_count: usize = byte_count.into();
        if byte_count > MAX_USER_AGENT_LENGTH {
            return Err(SerializationError::Parse(
                "user agent too long: must be 256 bytes or less",
            ));
        }
        let user_agent = zcash_deserialize_string_external_count(byte_count, &mut reader)?;

        let start_height = block::Height(reader.read_u32::<LittleEndian>()?);

        let relay = read_flag(&mut reader, "relay must be 0 or 1")?;
        let announce = read_flag(&mut reader, "announce must be 0 or 1")?;
        let full_ids = read_flag(&mut reader, "full_ids must be 0 or 1")?;

        Ok(InitRecord {
            version,
            services,
            nonce,
            user_agent,
            start_height,
            relay,
            announce,
            full_ids,
        })
    }
}

/// Reads a `uint8` flag that must be 0 or 1 (draft, "Handshake Validation").
fn read_flag<R: Read>(reader: &mut R, invalid: &'static str) -> Result<bool, SerializationError> {
    match reader.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SerializationError::Parse(invalid)),
    }
}
