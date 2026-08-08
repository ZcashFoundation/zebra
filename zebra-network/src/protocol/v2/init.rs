//! The `init` record of the version 2 Zcash P2P network protocol handshake.

use std::io;

use byteorder::{LittleEndian, ReadBytesExt};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use zebra_chain::{
    block,
    serialization::{CompactSize64, ZcashDeserialize, ZcashSerialize},
};

use crate::protocol::external::{
    types::{Nonce, PeerServices, Version},
    zcash_deserialize_user_agent,
};

use super::{constants::HANDSHAKE_RECORD_KIND_INIT, record, types::WireError};

/// A record on the handshake stream.
///
/// A node must ignore handshake-stream records whose kind it does not
/// recognize, so that future record kinds can be introduced without version
/// gating.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HandshakeRecord {
    /// The `init` record (kind `0x00`).
    Init(InitRecord),

    /// A record of an unrecognized kind, which is ignored.
    Unknown(u8),
}

/// The `init` record exchanged on the handshake stream when a connection is
/// established.
///
/// The legacy `timestamp`, `addr_recv`, and `addr_from` fields have no
/// equivalent: they served clock sanity checks and address self-discovery
/// functions that are out of scope for the version 2 protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitRecord {
    /// The sender's advertised protocol version.
    pub version: Version,

    /// The services advertised by the sender.
    pub services: PeerServices,

    /// A random nonce for self-connection detection.
    pub nonce: Nonce,

    /// The sender's user agent string (at most `MAX_USER_AGENT_LENGTH`
    /// bytes, the same limit as legacy `version` messages).
    pub user_agent: String,

    /// The best block height known to the sender.
    pub start_height: block::Height,

    /// Whether the sender wants transaction relay.
    ///
    /// If false, the peer must not open a transaction announcement stream to
    /// the sender, and should not announce transactions to it by any other
    /// means.
    pub relay: bool,

    /// Whether the sender requests high-bandwidth compact block
    /// announcements.
    pub announce: bool,

    /// Whether the sender requests full transaction IDs in compact block
    /// announcements.
    pub full_ids: bool,
}

impl InitRecord {
    /// Encodes this `init` record as a handshake-stream record payload,
    /// including the leading record kind byte.
    pub fn to_record_payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(32 + self.user_agent.len());

        payload.push(HANDSHAKE_RECORD_KIND_INIT);
        payload.extend_from_slice(&self.version.0.to_le_bytes());
        CompactSize64::from(self.services.bits())
            .zcash_serialize(&mut payload)
            .expect("writing to a Vec never fails");
        payload.extend_from_slice(&self.nonce.0.to_le_bytes());

        // Receivers enforce `MAX_USER_AGENT_LENGTH`; senders construct their
        // user agent from local configuration.
        self.user_agent
            .zcash_serialize(&mut payload)
            .expect("writing to a Vec never fails");

        payload.extend_from_slice(&self.start_height.0.to_le_bytes());
        payload.push(self.relay.into());
        payload.push(self.announce.into());
        payload.push(self.full_ids.into());

        payload
    }

    /// Writes this `init` record to `writer` as a length-prefixed
    /// handshake-stream record, and flushes the writer.
    pub async fn write<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), WireError> {
        let mut record = Vec::new();
        record::write_record(&mut record, &self.to_record_payload())?;
        writer.write_all(&record).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Reads the next record from the handshake stream, skipping records of
    /// unrecognized kinds, until an `init` record arrives.
    ///
    /// Returns `Ok(None)` if the stream was finished before an `init` record
    /// arrived: the peer is signalling intent to disconnect.
    pub async fn read<R: AsyncRead + Unpin>(
        reader: &mut R,
    ) -> Result<Option<InitRecord>, WireError> {
        loop {
            let payload = match record::read_record(reader).await? {
                Some(payload) => payload,
                None => return Ok(None),
            };

            match HandshakeRecord::parse(&payload)? {
                HandshakeRecord::Init(init) => return Ok(Some(init)),
                HandshakeRecord::Unknown(_kind) => continue,
            }
        }
    }
}

impl HandshakeRecord {
    /// Parses a handshake-stream record payload.
    pub fn parse(payload: &[u8]) -> Result<Self, WireError> {
        let (&kind, fields) = payload.split_first().ok_or_else(|| {
            WireError::Protocol("empty record on the handshake stream".to_string())
        })?;

        if kind != HANDSHAKE_RECORD_KIND_INIT {
            return Ok(HandshakeRecord::Unknown(kind));
        }

        let mut reader = io::Cursor::new(fields);

        let version = Version(reader.read_u32::<LittleEndian>()?);
        let services = CompactSize64::zcash_deserialize(&mut reader)?;
        let services = PeerServices::from_bits_truncate(services.into());
        let nonce = Nonce(reader.read_u64::<LittleEndian>()?);

        let user_agent = read_user_agent(&mut reader)?;

        let start_height = block::Height(reader.read_u32::<LittleEndian>()?);
        let relay = read_bool_field(&mut reader, "relay")?;
        let announce = read_bool_field(&mut reader, "announce")?;
        let full_ids = read_bool_field(&mut reader, "full_ids")?;

        if reader.position() != fields.len() as u64 {
            return Err(WireError::Protocol(
                "trailing data in an init record".to_string(),
            ));
        }

        Ok(HandshakeRecord::Init(InitRecord {
            version,
            services,
            nonce,
            user_agent,
            start_height,
            relay,
            announce,
            full_ids,
        }))
    }
}

/// Reads the length-prefixed `user_agent` field, checking the length against
/// `MAX_USER_AGENT_LENGTH` before allocating, via the legacy `version`
/// message deserializer.
fn read_user_agent<R: io::Read>(reader: &mut R) -> Result<String, WireError> {
    zcash_deserialize_user_agent(reader)
        .map_err(|error| WireError::Protocol(format!("invalid user agent: {error}")))
}

/// Reads a field that must be exactly 0 or 1.
fn read_bool_field<R: io::Read>(reader: &mut R, name: &str) -> Result<bool, WireError> {
    match reader.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(WireError::Protocol(format!(
            "invalid value {value} for the {name} field: must be 0 or 1",
        ))),
    }
}
