//! A Tokio codec mapping byte streams to Bitcoin message streams.

use std::{
    cmp::min,
    convert::TryInto,
    fmt,
    io::{Cursor, Read, Write},
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use bytes::{BufMut, BytesMut};
use chrono::{TimeZone, Utc};
use tokio_util::codec::{Decoder, Encoder};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::{
        sha256d, zcash_deserialize_bytes_external_count, FakeWriter, ReadZcashExt,
        SerializationError as Error, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
        MAX_PROTOCOL_MESSAGE_LEN,
    },
    transaction::Transaction,
};

use crate::constants;

use super::{
    addr::{AddrInVersion, AddrV1, AddrV2},
    message::{Message, RejectReason},
    types::*,
};

/// The length of a Bitcoin message header.
const HEADER_LEN: usize = 24usize;

/// A codec which produces Bitcoin messages from byte streams and vice versa.
pub struct Codec {
    builder: Builder,
    state: DecodeState,
}

/// A builder for specifying [`Codec`] options.
pub struct Builder {
    /// The network magic to use in encoding.
    network: Network,
    /// The protocol version to speak when encoding/decoding.
    version: Version,
    /// The maximum allowable message length.
    max_len: usize,
    /// An optional address label, to use for reporting metrics.
    metrics_addr_label: Option<String>,
}

impl Codec {
    /// Return a builder for constructing a [`Codec`].
    pub fn builder() -> Builder {
        Builder {
            network: Network::Mainnet,
            version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
            max_len: MAX_PROTOCOL_MESSAGE_LEN,
            metrics_addr_label: None,
        }
    }

    /// Reconfigure the version used by the codec, e.g., after completing a handshake.
    pub fn reconfigure_version(&mut self, version: Version) {
        self.builder.version = version;
    }
}

impl Builder {
    /// Finalize the builder and return a [`Codec`].
    pub fn finish(self) -> Codec {
        Codec {
            builder: self,
            state: DecodeState::Head,
        }
    }

    /// Configure the codec for the given [`Network`].
    pub fn for_network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    /// Configure the codec for the given [`Version`].
    #[allow(dead_code)]
    pub fn for_version(mut self, version: Version) -> Self {
        self.version = version;
        self
    }

    /// Configure the codec's maximum accepted payload size, in bytes.
    #[allow(dead_code)]
    pub fn with_max_body_len(mut self, len: usize) -> Self {
        self.max_len = len;
        self
    }

    /// Configure the codec with a label corresponding to the peer address.
    pub fn with_metrics_addr_label(mut self, metrics_addr_label: String) -> Self {
        self.metrics_addr_label = Some(metrics_addr_label);
        self
    }
}

// ======== Encoding =========

impl Encoder<Message> for Codec {
    type Error = Error;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        use Error::Parse;

        let body_length = self.body_length(&item);

        if body_length > self.builder.max_len {
            return Err(Parse("body length exceeded maximum size"));
        }

        if let Some(addr_label) = self.builder.metrics_addr_label.clone() {
            metrics::counter!("zcash.net.out.bytes.total",
                              (body_length + HEADER_LEN) as u64,
                              "addr" => addr_label);
        }

        use Message::*;
        // Note: because all match arms must have
        // the same type, and the array length is
        // part of the type, having at least one
        // of length 12 checks that they are all
        // of length 12, as they must be &[u8; 12].
        let command = match item {
            Version { .. } => b"version\0\0\0\0\0",
            Verack { .. } => b"verack\0\0\0\0\0\0",
            Ping { .. } => b"ping\0\0\0\0\0\0\0\0",
            Pong { .. } => b"pong\0\0\0\0\0\0\0\0",
            Reject { .. } => b"reject\0\0\0\0\0\0",
            Addr { .. } => b"addr\0\0\0\0\0\0\0\0",
            GetAddr { .. } => b"getaddr\0\0\0\0\0",
            Block { .. } => b"block\0\0\0\0\0\0\0",
            GetBlocks { .. } => b"getblocks\0\0\0",
            Headers { .. } => b"headers\0\0\0\0\0",
            GetHeaders { .. } => b"getheaders\0\0",
            Inv { .. } => b"inv\0\0\0\0\0\0\0\0\0",
            GetData { .. } => b"getdata\0\0\0\0\0",
            NotFound { .. } => b"notfound\0\0\0\0",
            Tx { .. } => b"tx\0\0\0\0\0\0\0\0\0\0",
            Mempool { .. } => b"mempool\0\0\0\0\0",
            FilterLoad { .. } => b"filterload\0\0",
            FilterAdd { .. } => b"filteradd\0\0\0",
            FilterClear { .. } => b"filterclear\0",
        };
        trace!(?item, len = body_length);

        dst.reserve(HEADER_LEN + body_length);
        let start_len = dst.len();
        {
            let dst = &mut dst.writer();
            dst.write_all(&Magic::from(self.builder.network).0[..])?;
            dst.write_all(command)?;
            dst.write_u32::<LittleEndian>(body_length as u32)?;

            // We zero the checksum at first, and compute it later
            // after the body has been written.
            dst.write_u32::<LittleEndian>(0)?;

            self.write_body(&item, dst)?;
        }
        let checksum = sha256d::Checksum::from(&dst[start_len + HEADER_LEN..]);
        dst[start_len + 20..][..4].copy_from_slice(&checksum.0);

        Ok(())
    }
}

impl Codec {
    /// Obtain the size of the body of a given message. This will match the
    /// number of bytes written to the writer provided to `write_body` for the
    /// same message.
    ///
    /// TODO: Replace with a size estimate, to avoid multiple serializations
    /// for large data structures like lists, blocks, and transactions.
    /// See #1774.
    fn body_length(&self, msg: &Message) -> usize {
        let mut writer = FakeWriter(0);

        self.write_body(msg, &mut writer)
            .expect("writer should never fail");
        writer.0
    }

    /// Write the body of the message into the given writer. This allows writing
    /// the message body prior to writing the header, so that the header can
    /// contain a checksum of the message body.
    fn write_body<W: Write>(&self, msg: &Message, mut writer: W) -> Result<(), Error> {
        match msg {
            Message::Version {
                version,
                services,
                timestamp,
                address_recv,
                address_from,
                nonce,
                user_agent,
                start_height,
                relay,
            } => {
                writer.write_u32::<LittleEndian>(version.0)?;
                writer.write_u64::<LittleEndian>(services.bits())?;
                // # Security
                // DateTime<Utc>::timestamp has a smaller range than i64, so
                // serialization can not error.
                writer.write_i64::<LittleEndian>(timestamp.timestamp())?;

                address_recv.zcash_serialize(&mut writer)?;
                address_from.zcash_serialize(&mut writer)?;

                writer.write_u64::<LittleEndian>(nonce.0)?;
                user_agent.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(start_height.0)?;
                writer.write_u8(*relay as u8)?;
            }
            Message::Verack => { /* Empty payload -- no-op */ }
            Message::Ping(nonce) => {
                writer.write_u64::<LittleEndian>(nonce.0)?;
            }
            Message::Pong(nonce) => {
                writer.write_u64::<LittleEndian>(nonce.0)?;
            }
            Message::Reject {
                message,
                ccode,
                reason,
                data,
            } => {
                message.zcash_serialize(&mut writer)?;
                writer.write_u8(*ccode as u8)?;
                reason.zcash_serialize(&mut writer)?;
                if let Some(data) = data {
                    writer.write_all(data)?;
                }
            }
            Message::Addr(addrs) => {
                assert!(
                    addrs.len() <= constants::MAX_ADDRS_IN_MESSAGE,
                    "unexpectedly large Addr message: greater than MAX_ADDRS_IN_MESSAGE addresses"
                );

                // Regardless of the way we received the address,
                // Zebra always sends `addr` messages
                let v1_addrs: Vec<AddrV1> = addrs.iter().map(|addr| AddrV1::from(*addr)).collect();
                v1_addrs.zcash_serialize(&mut writer)?
            }
            Message::GetAddr => { /* Empty payload -- no-op */ }
            Message::Block(block) => block.zcash_serialize(&mut writer)?,
            Message::GetBlocks { known_blocks, stop } => {
                writer.write_u32::<LittleEndian>(self.builder.version.0)?;
                known_blocks.zcash_serialize(&mut writer)?;
                stop.unwrap_or(block::Hash([0; 32]))
                    .zcash_serialize(&mut writer)?;
            }
            Message::GetHeaders { known_blocks, stop } => {
                writer.write_u32::<LittleEndian>(self.builder.version.0)?;
                known_blocks.zcash_serialize(&mut writer)?;
                stop.unwrap_or(block::Hash([0; 32]))
                    .zcash_serialize(&mut writer)?;
            }
            Message::Headers(headers) => headers.zcash_serialize(&mut writer)?,
            Message::Inv(hashes) => hashes.zcash_serialize(&mut writer)?,
            Message::GetData(hashes) => hashes.zcash_serialize(&mut writer)?,
            Message::NotFound(hashes) => hashes.zcash_serialize(&mut writer)?,
            Message::Tx(transaction) => transaction.transaction.zcash_serialize(&mut writer)?,
            Message::Mempool => { /* Empty payload -- no-op */ }
            Message::FilterLoad {
                filter,
                hash_functions_count,
                tweak,
                flags,
            } => {
                writer.write_all(&filter.0)?;
                writer.write_u32::<LittleEndian>(*hash_functions_count)?;
                writer.write_u32::<LittleEndian>(tweak.0)?;
                writer.write_u8(*flags)?;
            }
            Message::FilterAdd { data } => {
                writer.write_all(data)?;
            }
            Message::FilterClear => { /* Empty payload -- no-op */ }
        }
        Ok(())
    }
}

// ======== Decoding =========

enum DecodeState {
    Head,
    Body {
        body_len: usize,
        command: [u8; 12],
        checksum: sha256d::Checksum,
    },
}

impl fmt::Debug for DecodeState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DecodeState::Head => write!(f, "DecodeState::Head"),
            DecodeState::Body {
                body_len,
                command,
                checksum,
            } => f
                .debug_struct("DecodeState::Body")
                .field("body_len", &body_len)
                .field("command", &String::from_utf8_lossy(command))
                .field("checksum", &checksum)
                .finish(),
        }
    }
}

impl Decoder for Codec {
    type Item = Message;
    type Error = Error;

    #[allow(clippy::unwrap_in_result)]
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        use Error::Parse;
        match self.state {
            DecodeState::Head => {
                // First check that the src buffer contains an entire header.
                if src.len() < HEADER_LEN {
                    trace!(?self.state, "src buffer does not have an entire header, waiting");
                    // Signal that decoding requires more data.
                    return Ok(None);
                }

                // Now that we know that src contains a header, split off the header section.
                let header = src.split_to(HEADER_LEN);

                // Create a cursor over the header and parse its fields.
                let mut header_reader = Cursor::new(&header);
                let magic = Magic(header_reader.read_4_bytes()?);
                let command = header_reader.read_12_bytes()?;
                let body_len = header_reader.read_u32::<LittleEndian>()? as usize;
                let checksum = sha256d::Checksum(header_reader.read_4_bytes()?);
                trace!(
                    ?self.state,
                    ?magic,
                    command = %String::from_utf8(
                        command.iter()
                            .cloned()
                            .flat_map(std::ascii::escape_default)
                            .collect()
                    ).unwrap(),
                    body_len,
                    ?checksum,
                    "read header from src buffer"
                );

                if magic != Magic::from(self.builder.network) {
                    return Err(Parse("supplied magic did not meet expectations"));
                }
                if body_len > self.builder.max_len {
                    return Err(Parse("body length exceeded maximum size"));
                }

                if let Some(label) = self.builder.metrics_addr_label.clone() {
                    metrics::counter!("zcash.net.in.bytes.total", (body_len + HEADER_LEN) as u64, "addr" =>  label);
                }

                // Reserve buffer space for the expected body and the following header.
                src.reserve(body_len + HEADER_LEN);

                self.state = DecodeState::Body {
                    body_len,
                    command,
                    checksum,
                };

                // Now that the state is updated, recurse to attempt body decoding.
                self.decode(src)
            }
            DecodeState::Body {
                body_len,
                command,
                checksum,
            } => {
                if src.len() < body_len {
                    // Need to wait for the full body
                    trace!(?self.state, len = src.len(), "src buffer does not have an entire body, waiting");
                    return Ok(None);
                }

                // Now that we know we have the full body, split off the body,
                // and reset the decoder state for the next message. Otherwise
                // we will attempt to read the next header as the current body.
                let body = src.split_to(body_len);
                self.state = DecodeState::Head;

                if checksum != sha256d::Checksum::from(&body[..]) {
                    return Err(Parse(
                        "supplied message checksum does not match computed checksum",
                    ));
                }

                let mut body_reader = Cursor::new(&body);
                match &command {
                    b"version\0\0\0\0\0" => self.read_version(&mut body_reader),
                    b"verack\0\0\0\0\0\0" => self.read_verack(&mut body_reader),
                    b"ping\0\0\0\0\0\0\0\0" => self.read_ping(&mut body_reader),
                    b"pong\0\0\0\0\0\0\0\0" => self.read_pong(&mut body_reader),
                    b"reject\0\0\0\0\0\0" => self.read_reject(&mut body_reader),
                    b"addr\0\0\0\0\0\0\0\0" => self.read_addr(&mut body_reader),
                    b"addrv2\0\0\0\0\0\0" => self.read_addrv2(&mut body_reader),
                    b"getaddr\0\0\0\0\0" => self.read_getaddr(&mut body_reader),
                    b"block\0\0\0\0\0\0\0" => self.read_block(&mut body_reader),
                    b"getblocks\0\0\0" => self.read_getblocks(&mut body_reader),
                    b"headers\0\0\0\0\0" => self.read_headers(&mut body_reader),
                    b"getheaders\0\0" => self.read_getheaders(&mut body_reader),
                    b"inv\0\0\0\0\0\0\0\0\0" => self.read_inv(&mut body_reader),
                    b"getdata\0\0\0\0\0" => self.read_getdata(&mut body_reader),
                    b"notfound\0\0\0\0" => self.read_notfound(&mut body_reader),
                    b"tx\0\0\0\0\0\0\0\0\0\0" => self.read_tx(&mut body_reader),
                    b"mempool\0\0\0\0\0" => self.read_mempool(&mut body_reader),
                    b"filterload\0\0" => self.read_filterload(&mut body_reader, body_len),
                    b"filteradd\0\0\0" => self.read_filteradd(&mut body_reader, body_len),
                    b"filterclear\0" => self.read_filterclear(&mut body_reader),
                    _ => {
                        let command_string = String::from_utf8_lossy(&command);

                        // # Security
                        //
                        // Zcash connections are not authenticated, so malicious nodes can
                        // send fake messages, with connected peers' IP addresses in the IP header.
                        //
                        // Since we can't verify their source, Zebra needs to ignore unexpected messages,
                        // because closing the connection could cause a denial of service or eclipse attack.
                        debug!(?command, %command_string, "unknown message command from peer");
                        return Ok(None);
                    }
                }
                // We need Ok(Some(msg)) to signal that we're done decoding.
                // This is also convenient for tracing the parse result.
                .map(|msg| {
                    // bitcoin allows extra data at the end of most messages,
                    // so that old nodes can still read newer message formats,
                    // and ignore any extra fields
                    let extra_bytes = body.len() as u64 - body_reader.position();
                    if extra_bytes == 0 {
                        trace!(?extra_bytes, %msg, "finished message decoding");
                    } else {
                        // log when there are extra bytes, so we know when we need to
                        // upgrade message formats
                        debug!(?extra_bytes, %msg, "extra data after decoding message");
                    }
                    Some(msg)
                })
            }
        }
    }
}

impl Codec {
    fn read_version<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::Version {
            version: Version(reader.read_u32::<LittleEndian>()?),
            // Use from_bits_truncate to discard unknown service bits.
            services: PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?),
            timestamp: Utc
                .timestamp_opt(reader.read_i64::<LittleEndian>()?, 0)
                .single()
                .ok_or(Error::Parse(
                    "version timestamp is out of range for DateTime",
                ))?,
            address_recv: AddrInVersion::zcash_deserialize(&mut reader)?,
            address_from: AddrInVersion::zcash_deserialize(&mut reader)?,
            nonce: Nonce(reader.read_u64::<LittleEndian>()?),
            user_agent: String::zcash_deserialize(&mut reader)?,
            start_height: block::Height(reader.read_u32::<LittleEndian>()?),
            relay: match reader.read_u8()? {
                0 => false,
                1 => true,
                _ => return Err(Error::Parse("non-bool value supplied in relay field")),
            },
        })
    }

    fn read_verack<R: Read>(&self, mut _reader: R) -> Result<Message, Error> {
        Ok(Message::Verack)
    }

    fn read_ping<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::Ping(Nonce(reader.read_u64::<LittleEndian>()?)))
    }

    fn read_pong<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::Pong(Nonce(reader.read_u64::<LittleEndian>()?)))
    }

    fn read_reject<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::Reject {
            message: String::zcash_deserialize(&mut reader)?,
            ccode: match reader.read_u8()? {
                0x01 => RejectReason::Malformed,
                0x10 => RejectReason::Invalid,
                0x11 => RejectReason::Obsolete,
                0x12 => RejectReason::Duplicate,
                0x40 => RejectReason::Nonstandard,
                0x41 => RejectReason::Dust,
                0x42 => RejectReason::InsufficientFee,
                0x43 => RejectReason::Checkpoint,
                0x50 => RejectReason::Other,
                _ => return Err(Error::Parse("invalid RejectReason value in ccode field")),
            },
            reason: String::zcash_deserialize(&mut reader)?,
            // Sometimes there's data, sometimes there isn't. There's no length
            // field, this is just implicitly encoded by the body_len.
            // Apparently all existing implementations only supply 32 bytes of
            // data (hash identifying the rejected object) or none (and we model
            // the Reject message that way), so instead of passing in the
            // body_len separately and calculating remaining bytes, just try to
            // read 32 bytes and ignore any failures. (The caller will log and
            // ignore any trailing bytes.)
            data: reader.read_32_bytes().ok(),
        })
    }

    /// Deserialize an `addr` (v1) message into a list of `MetaAddr`s.
    pub(super) fn read_addr<R: Read>(&self, reader: R) -> Result<Message, Error> {
        let addrs: Vec<AddrV1> = reader.zcash_deserialize_into()?;

        if addrs.len() > constants::MAX_ADDRS_IN_MESSAGE {
            return Err(Error::Parse(
                "more than MAX_ADDRS_IN_MESSAGE in addr message",
            ));
        }

        // Convert the received address format to Zebra's internal `MetaAddr`.
        let addrs = addrs.into_iter().map(Into::into).collect();
        Ok(Message::Addr(addrs))
    }

    /// Deserialize an `addrv2` message into a list of `MetaAddr`s.
    ///
    /// Currently, Zebra parses received `addrv2`s, ignoring some address types.
    /// Zebra never sends `addrv2` messages.
    pub(super) fn read_addrv2<R: Read>(&self, reader: R) -> Result<Message, Error> {
        let addrs: Vec<AddrV2> = reader.zcash_deserialize_into()?;

        if addrs.len() > constants::MAX_ADDRS_IN_MESSAGE {
            return Err(Error::Parse(
                "more than MAX_ADDRS_IN_MESSAGE in addrv2 message",
            ));
        }

        // Convert the received address format to Zebra's internal `MetaAddr`,
        // ignoring unsupported network IDs.
        let addrs = addrs
            .into_iter()
            .filter_map(|addr| addr.try_into().ok())
            .collect();
        Ok(Message::Addr(addrs))
    }

    fn read_getaddr<R: Read>(&self, mut _reader: R) -> Result<Message, Error> {
        Ok(Message::GetAddr)
    }

    fn read_block<R: Read + std::marker::Send>(&self, reader: R) -> Result<Message, Error> {
        let result = Self::deserialize_block_spawning(reader);
        Ok(Message::Block(result?.into()))
    }

    fn read_getblocks<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        if self.builder.version == Version(reader.read_u32::<LittleEndian>()?) {
            let known_blocks = Vec::zcash_deserialize(&mut reader)?;
            let stop_hash = block::Hash::zcash_deserialize(&mut reader)?;
            let stop = if stop_hash != block::Hash([0; 32]) {
                Some(stop_hash)
            } else {
                None
            };
            Ok(Message::GetBlocks { known_blocks, stop })
        } else {
            Err(Error::Parse("getblocks version did not match negotiation"))
        }
    }

    /// Deserialize a `headers` message.
    ///
    /// See [Zcash block header] for the enumeration of these fields.
    ///
    /// [Zcash block header](https://zips.z.cash/protocol/protocol.pdf#page=84)
    fn read_headers<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::Headers(Vec::zcash_deserialize(&mut reader)?))
    }

    fn read_getheaders<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        if self.builder.version == Version(reader.read_u32::<LittleEndian>()?) {
            let known_blocks = Vec::zcash_deserialize(&mut reader)?;
            let stop_hash = block::Hash::zcash_deserialize(&mut reader)?;
            let stop = if stop_hash != block::Hash([0; 32]) {
                Some(stop_hash)
            } else {
                None
            };
            Ok(Message::GetHeaders { known_blocks, stop })
        } else {
            Err(Error::Parse("getblocks version did not match negotiation"))
        }
    }

    fn read_inv<R: Read>(&self, reader: R) -> Result<Message, Error> {
        Ok(Message::Inv(Vec::zcash_deserialize(reader)?))
    }

    fn read_getdata<R: Read>(&self, reader: R) -> Result<Message, Error> {
        Ok(Message::GetData(Vec::zcash_deserialize(reader)?))
    }

    fn read_notfound<R: Read>(&self, reader: R) -> Result<Message, Error> {
        Ok(Message::NotFound(Vec::zcash_deserialize(reader)?))
    }

    fn read_tx<R: Read + std::marker::Send>(&self, reader: R) -> Result<Message, Error> {
        let result = Self::deserialize_transaction_spawning(reader);
        Ok(Message::Tx(result?.into()))
    }

    fn read_mempool<R: Read>(&self, mut _reader: R) -> Result<Message, Error> {
        Ok(Message::Mempool)
    }

    fn read_filterload<R: Read>(&self, mut reader: R, body_len: usize) -> Result<Message, Error> {
        // The maximum length of a filter.
        const MAX_FILTERLOAD_FILTER_LENGTH: usize = 36000;

        // The data length of the fields:
        // hash_functions_count + tweak + flags.
        const FILTERLOAD_FIELDS_LENGTH: usize = 4 + 4 + 1;

        // The maximum length of a filter message's data.
        const MAX_FILTERLOAD_MESSAGE_LENGTH: usize =
            MAX_FILTERLOAD_FILTER_LENGTH + FILTERLOAD_FIELDS_LENGTH;

        if !(FILTERLOAD_FIELDS_LENGTH..=MAX_FILTERLOAD_MESSAGE_LENGTH).contains(&body_len) {
            return Err(Error::Parse("Invalid filterload message body length."));
        }

        // Memory Denial of Service: we just checked the untrusted parsed length
        let filter_length: usize = body_len - FILTERLOAD_FIELDS_LENGTH;
        let filter_bytes = zcash_deserialize_bytes_external_count(filter_length, &mut reader)?;

        Ok(Message::FilterLoad {
            filter: Filter(filter_bytes),
            hash_functions_count: reader.read_u32::<LittleEndian>()?,
            tweak: Tweak(reader.read_u32::<LittleEndian>()?),
            flags: reader.read_u8()?,
        })
    }

    fn read_filteradd<R: Read>(&self, mut reader: R, body_len: usize) -> Result<Message, Error> {
        const MAX_FILTERADD_LENGTH: usize = 520;

        // Memory Denial of Service: limit the untrusted parsed length
        let filter_length: usize = min(body_len, MAX_FILTERADD_LENGTH);
        let filter_bytes = zcash_deserialize_bytes_external_count(filter_length, &mut reader)?;

        Ok(Message::FilterAdd { data: filter_bytes })
    }

    fn read_filterclear<R: Read>(&self, mut _reader: R) -> Result<Message, Error> {
        Ok(Message::FilterClear)
    }

    /// Given the reader, deserialize the transaction in the rayon thread pool.
    #[allow(clippy::unwrap_in_result)]
    fn deserialize_transaction_spawning<R: Read + std::marker::Send>(
        reader: R,
    ) -> Result<Transaction, Error> {
        let mut result = None;

        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // Since we use `block_in_place()`, other futures running on the connection task will be blocked:
        // https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html
        //
        // We can't use `spawn_blocking()` because:
        // - The `reader` has a lifetime (but we could replace it with a `Vec` of message data)
        // - There is no way to check the blocking task's future for panics
        tokio::task::block_in_place(|| {
            rayon::in_place_scope_fifo(|s| {
                s.spawn_fifo(|_s| result = Some(Transaction::zcash_deserialize(reader)))
            })
        });

        result.expect("scope has already finished")
    }

    /// Given the reader, deserialize the block in the rayon thread pool.
    #[allow(clippy::unwrap_in_result)]
    fn deserialize_block_spawning<R: Read + std::marker::Send>(reader: R) -> Result<Block, Error> {
        let mut result = None;

        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // Since we use `block_in_place()`, other futures running on the connection task will be blocked:
        // https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html
        //
        // We can't use `spawn_blocking()` because:
        // - The `reader` has a lifetime (but we could replace it with a `Vec` of message data)
        // - There is no way to check the blocking task's future for panics
        tokio::task::block_in_place(|| {
            rayon::in_place_scope_fifo(|s| {
                s.spawn_fifo(|_s| result = Some(Block::zcash_deserialize(reader)))
            })
        });

        result.expect("scope has already finished")
    }
}

// XXX replace these interior unit tests with exterior integration tests + proptest
#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{MAX_DATETIME, MIN_DATETIME};
    use futures::prelude::*;
    use lazy_static::lazy_static;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    lazy_static! {
        static ref VERSION_TEST_VECTOR: Message = {
            let services = PeerServices::NODE_NETWORK;
            let timestamp = Utc.timestamp(1_568_000_000, 0);
            Message::Version {
                version: crate::constants::CURRENT_NETWORK_PROTOCOL_VERSION,
                services,
                timestamp,
                address_recv: AddrInVersion::new(
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6)), 8233),
                    services,
                ),
                address_from: AddrInVersion::new(
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6)), 8233),
                    services,
                ),
                nonce: Nonce(0x9082_4908_8927_9238),
                user_agent: "Zebra".to_owned(),
                start_height: block::Height(540_000),
                relay: true,
            }
        };
    }

    /// Check that the version test vector serializes and deserializes correctly
    #[test]
    fn version_message_round_trip() {
        let (rt, _init_guard) = zebra_test::init_async();

        let v = &*VERSION_TEST_VECTOR;

        use tokio_util::codec::{FramedRead, FramedWrite};
        let v_bytes = rt.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
                fw.send(v.clone())
                    .await
                    .expect("message should be serialized");
            }
            bytes
        });

        let v_parsed = rt.block_on(async {
            let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
            fr.next()
                .await
                .expect("a next message should be available")
                .expect("that message should deserialize")
        });

        assert_eq!(*v, v_parsed);
    }

    /// Check that version deserialization rejects out-of-range timestamps with
    /// an error.
    #[test]
    fn version_timestamp_out_of_range() {
        let v_err = deserialize_version_with_time(i64::MAX);
        assert!(
            matches!(v_err, Err(Error::Parse(_))),
            "expected error with version timestamp: {}",
            i64::MAX
        );

        let v_err = deserialize_version_with_time(i64::MIN);
        assert!(
            matches!(v_err, Err(Error::Parse(_))),
            "expected error with version timestamp: {}",
            i64::MIN
        );

        deserialize_version_with_time(1620777600).expect("recent time is valid");
        deserialize_version_with_time(0).expect("zero time is valid");
        deserialize_version_with_time(MIN_DATETIME.timestamp()).expect("min time is valid");
        deserialize_version_with_time(MAX_DATETIME.timestamp()).expect("max time is valid");
    }

    /// Deserialize a `Version` message containing `time`, and return the result.
    fn deserialize_version_with_time(time: i64) -> Result<Message, Error> {
        let (rt, _init_guard) = zebra_test::init_async();

        let v = &*VERSION_TEST_VECTOR;

        use tokio_util::codec::{FramedRead, FramedWrite};
        let v_bytes = rt.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
                fw.send(v.clone())
                    .await
                    .expect("message should be serialized");
            }

            let old_bytes = bytes.clone();

            // tweak the version bytes so the timestamp is set to `time`
            // Version serialization is specified at:
            // https://developer.bitcoin.org/reference/p2p_networking.html#version
            bytes[36..44].copy_from_slice(&time.to_le_bytes());

            // Checksum is specified at:
            // https://developer.bitcoin.org/reference/p2p_networking.html#message-headers
            let checksum = sha256d::Checksum::from(&bytes[HEADER_LEN..]);
            bytes[20..24].copy_from_slice(&checksum.0);

            debug!(?time,
                   old_len = ?old_bytes.len(), new_len = ?bytes.len(),
                   old_bytes = ?&old_bytes[36..44], new_bytes = ?&bytes[36..44]);

            bytes
        });

        rt.block_on(async {
            let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
            fr.next().await.expect("a next message should be available")
        })
    }

    #[test]
    fn filterload_message_round_trip() {
        let (rt, _init_guard) = zebra_test::init_async();

        let v = Message::FilterLoad {
            filter: Filter(vec![0; 35999]),
            hash_functions_count: 0,
            tweak: Tweak(0),
            flags: 0,
        };

        use tokio_util::codec::{FramedRead, FramedWrite};
        let v_bytes = rt.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
                fw.send(v.clone())
                    .await
                    .expect("message should be serialized");
            }
            bytes
        });

        let v_parsed = rt.block_on(async {
            let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
            fr.next()
                .await
                .expect("a next message should be available")
                .expect("that message should deserialize")
        });

        assert_eq!(v, v_parsed);
    }

    #[test]
    fn reject_message_no_extra_data_round_trip() {
        let (rt, _init_guard) = zebra_test::init_async();

        let v = Message::Reject {
            message: "experimental".to_string(),
            ccode: RejectReason::Malformed,
            reason: "message could not be decoded".to_string(),
            data: None,
        };

        use tokio_util::codec::{FramedRead, FramedWrite};
        let v_bytes = rt.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
                fw.send(v.clone())
                    .await
                    .expect("message should be serialized");
            }
            bytes
        });

        let v_parsed = rt.block_on(async {
            let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
            fr.next()
                .await
                .expect("a next message should be available")
                .expect("that message should deserialize")
        });

        assert_eq!(v, v_parsed);
    }

    #[test]
    fn reject_message_extra_data_round_trip() {
        let (rt, _init_guard) = zebra_test::init_async();

        let v = Message::Reject {
            message: "block".to_string(),
            ccode: RejectReason::Invalid,
            reason: "invalid block difficulty".to_string(),
            data: Some([0xff; 32]),
        };

        use tokio_util::codec::{FramedRead, FramedWrite};
        let v_bytes = rt.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
                fw.send(v.clone())
                    .await
                    .expect("message should be serialized");
            }
            bytes
        });

        let v_parsed = rt.block_on(async {
            let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
            fr.next()
                .await
                .expect("a next message should be available")
                .expect("that message should deserialize")
        });

        assert_eq!(v, v_parsed);
    }

    #[test]
    fn filterload_message_too_large_round_trip() {
        let (rt, _init_guard) = zebra_test::init_async();

        let v = Message::FilterLoad {
            filter: Filter(vec![0; 40000]),
            hash_functions_count: 0,
            tweak: Tweak(0),
            flags: 0,
        };

        use tokio_util::codec::{FramedRead, FramedWrite};
        let v_bytes = rt.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(&mut bytes, Codec::builder().finish());
                fw.send(v.clone())
                    .await
                    .expect("message should be serialized");
            }
            bytes
        });

        rt.block_on(async {
            let mut fr = FramedRead::new(Cursor::new(&v_bytes), Codec::builder().finish());
            fr.next()
                .await
                .expect("a next message should be available")
                .expect_err("that message should not deserialize")
        });
    }

    #[test]
    fn max_msg_size_round_trip() {
        use zebra_chain::serialization::ZcashDeserializeInto;

        //let (rt, _init_guard) = zebra_test::init_async();
        let _init_guard = zebra_test::init();

        // make tests with a Tx message
        let tx: Transaction = zebra_test::vectors::DUMMY_TX1
            .zcash_deserialize_into()
            .unwrap();
        let msg = Message::Tx(tx.into());

        use tokio_util::codec::{FramedRead, FramedWrite};

        // i know the above msg has a body of 85 bytes
        let size = 85;

        // reducing the max size to body size - 1
        zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(
                    &mut bytes,
                    Codec::builder().with_max_body_len(size - 1).finish(),
                );
                fw.send(msg.clone()).await.expect_err(
                    "message should not encode as it is bigger than the max allowed value",
                );
            }
        });

        // send again with the msg body size as max size
        let msg_bytes = zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
            let mut bytes = Vec::new();
            {
                let mut fw = FramedWrite::new(
                    &mut bytes,
                    Codec::builder().with_max_body_len(size).finish(),
                );
                fw.send(msg.clone())
                    .await
                    .expect("message should encode with the msg body size as max allowed value");
            }
            bytes
        });

        // receive with a reduced max size
        zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
            let mut fr = FramedRead::new(
                Cursor::new(&msg_bytes),
                Codec::builder().with_max_body_len(size - 1).finish(),
            );
            fr.next()
                .await
                .expect("a next message should be available")
                .expect_err("message should not decode as it is bigger than the max allowed value")
        });

        // receive again with the tx size as max size
        zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
            let mut fr = FramedRead::new(
                Cursor::new(&msg_bytes),
                Codec::builder().with_max_body_len(size).finish(),
            );
            fr.next()
                .await
                .expect("a next message should be available")
                .expect("message should decode with the msg body size as max allowed value")
        });
    }
}
