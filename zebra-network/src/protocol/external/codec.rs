//! A Tokio codec mapping byte streams to Bitcoin message streams.

use std::fmt;
use std::io::{Cursor, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use bytes::BytesMut;
use chrono::{TimeZone, Utc};
use tokio::codec::{Decoder, Encoder};

use zebra_chain::{
    block::{Block, BlockHeader, BlockHeaderHash},
    serialization::{
        ReadZcashExt, SerializationError as Error, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
    },
    transaction::Transaction,
    types::{BlockHeight, Sha256dChecksum},
};

use crate::{constants, types::Network};

use super::{
    inv::InventoryHash,
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
}

impl Codec {
    /// Return a builder for constructing a [`Codec`].
    pub fn builder() -> Builder {
        Builder {
            network: Network::Mainnet,
            version: constants::CURRENT_VERSION,
            max_len: 4_000_000,
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
    pub fn for_version(mut self, version: Version) -> Self {
        self.version = version;
        self
    }

    /// Configure the codec's maximum accepted payload size, in bytes.
    pub fn with_max_body_len(mut self, len: usize) -> Self {
        self.max_len = len;
        self
    }
}

// ======== Encoding =========

impl Encoder for Codec {
    type Item = Message;
    type Error = Error;

    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // XXX(HACK): this is inefficient and does an extra allocation.
        // instead, we should have a size estimator for the message, reserve
        // that much space, write the header (with zeroed checksum), then the body,
        // then write the computed checksum in-place.  for now, just do an extra alloc.

        let mut body = Vec::new();
        self.write_body(&item, &mut body)?;

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
        trace!(?item, len = body.len());

        // XXX this should write directly into the buffer,
        // but leave it for now until we fix the issue above.
        let mut header = [0u8; HEADER_LEN];
        let mut header_writer = Cursor::new(&mut header[..]);
        header_writer.write_all(&self.builder.network.magic().0)?;
        header_writer.write_all(command)?;
        header_writer.write_u32::<LittleEndian>(body.len() as u32)?;
        header_writer.write_all(&Sha256dChecksum::from(&body[..]).0)?;

        dst.reserve(HEADER_LEN + body.len());
        dst.extend_from_slice(&header);
        dst.extend_from_slice(&body);

        Ok(())
    }
}

impl Codec {
    /// Write the body of the message into the given writer. This allows writing
    /// the message body prior to writing the header, so that the header can
    /// contain a checksum of the message body.
    fn write_body<W: Write>(&self, msg: &Message, mut writer: W) -> Result<(), Error> {
        use Message::*;
        match *msg {
            Version {
                ref version,
                ref services,
                ref timestamp,
                ref address_recv,
                ref address_from,
                ref nonce,
                ref user_agent,
                ref start_height,
                ref relay,
            } => {
                writer.write_u32::<LittleEndian>(version.0)?;
                writer.write_u64::<LittleEndian>(services.bits())?;
                writer.write_i64::<LittleEndian>(timestamp.timestamp())?;

                let (recv_services, recv_addr) = address_recv;
                writer.write_u64::<LittleEndian>(recv_services.bits())?;
                writer.write_socket_addr(*recv_addr)?;

                let (from_services, from_addr) = address_from;
                writer.write_u64::<LittleEndian>(from_services.bits())?;
                writer.write_socket_addr(*from_addr)?;

                writer.write_u64::<LittleEndian>(nonce.0)?;
                writer.write_string(&user_agent)?;
                writer.write_u32::<LittleEndian>(start_height.0)?;
                writer.write_u8(*relay as u8)?;
            }
            Verack => { /* Empty payload -- no-op */ }
            Ping(nonce) => {
                writer.write_u64::<LittleEndian>(nonce.0)?;
            }
            Pong(nonce) => {
                writer.write_u64::<LittleEndian>(nonce.0)?;
            }
            Reject {
                ref message,
                ref ccode,
                ref reason,
                ref data,
            } => {
                writer.write_string(&message)?;
                writer.write_u8(*ccode as u8)?;
                writer.write_string(&reason)?;
                writer.write_all(&data.unwrap())?;
            }
            Addr(ref addrs) => {
                writer.write_compactsize(addrs.len() as u64)?;
                for addr in addrs {
                    addr.zcash_serialize(&mut writer)?;
                }
            }
            GetAddr => { /* Empty payload -- no-op */ }
            Block {
                ref version,
                ref block,
            } => {
                writer.write_u32::<LittleEndian>(version.0)?;
                block.zcash_serialize(&mut writer)?
            }
            GetBlocks {
                ref version,
                ref block_locator_hashes,
                ref hash_stop,
            } => {
                writer.write_u32::<LittleEndian>(version.0)?;
                writer.write_compactsize(block_locator_hashes.len() as u64)?;
                for hash in block_locator_hashes {
                    hash.zcash_serialize(&mut writer)?;
                }
                hash_stop.zcash_serialize(&mut writer)?;
            }
            GetHeaders {
                ref version,
                ref block_locator_hashes,
                ref hash_stop,
            } => {
                writer.write_u32::<LittleEndian>(version.0)?;
                writer.write_compactsize(block_locator_hashes.len() as u64)?;
                for hash in block_locator_hashes {
                    hash.zcash_serialize(&mut writer)?;
                }
                hash_stop.zcash_serialize(&mut writer)?;
            }
            Headers(ref headers) => {
                writer.write_compactsize(headers.len() as u64)?;
                for header in headers {
                    header.zcash_serialize(&mut writer)?;
                }
            }
            Inv(ref hashes) => {
                writer.write_compactsize(hashes.len() as u64)?;
                for hash in hashes {
                    hash.zcash_serialize(&mut writer)?;
                }
            }
            GetData(ref hashes) => {
                writer.write_compactsize(hashes.len() as u64)?;
                for hash in hashes {
                    hash.zcash_serialize(&mut writer)?;
                }
            }
            NotFound(ref hashes) => {
                writer.write_compactsize(hashes.len() as u64)?;
                for hash in hashes {
                    hash.zcash_serialize(&mut writer)?;
                }
            }
            Tx {
                ref version,
                ref transaction,
            } => {
                writer.write_u32::<LittleEndian>(version.0)?;
                transaction.zcash_serialize(&mut writer)?
            }
            Mempool => { /* Empty payload -- no-op */ }
            FilterLoad {
                ref filter,
                ref hash_functions_count,
                ref tweak,
                ref flags,
            } => {
                filter.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(*hash_functions_count)?;
                writer.write_u32::<LittleEndian>(tweak.0)?;
                writer.write_u8(*flags)?;
            }
            FilterAdd { ref data } => {
                writer.write_all(data)?;
            }
            FilterClear => { /* Empty payload -- no-op */ }
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
        checksum: Sha256dChecksum,
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
                let checksum = Sha256dChecksum(header_reader.read_4_bytes()?);
                trace!(
                    ?self.state,
                    ?magic,
                    command = %String::from_utf8_lossy(&command),
                    body_len,
                    ?checksum,
                    "read header from src buffer"
                );

                if magic != self.builder.network.magic() {
                    return Err(Parse("supplied magic did not meet expectations"));
                }
                if body_len >= self.builder.max_len {
                    return Err(Parse("body length exceeded maximum size"));
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

                if checksum != Sha256dChecksum::from(&body[..]) {
                    return Err(Parse(
                        "supplied message checksum does not match computed checksum",
                    ));
                }

                let body_reader = Cursor::new(&body);
                match &command {
                    b"version\0\0\0\0\0" => self.read_version(body_reader),
                    b"verack\0\0\0\0\0\0" => self.read_verack(body_reader),
                    b"ping\0\0\0\0\0\0\0\0" => self.read_ping(body_reader),
                    b"pong\0\0\0\0\0\0\0\0" => self.read_pong(body_reader),
                    b"reject\0\0\0\0\0\0" => self.read_reject(body_reader),
                    b"addr\0\0\0\0\0\0\0\0" => self.read_addr(body_reader),
                    b"getaddr\0\0\0\0\0" => self.read_getaddr(body_reader),
                    b"block\0\0\0\0\0\0\0" => self.read_block(body_reader),
                    b"getblocks\0\0\0" => self.read_getblocks(body_reader),
                    b"headers\0\0\0\0\0" => self.read_headers(body_reader),
                    b"getheaders\0\0" => self.read_getheaders(body_reader),
                    b"inv\0\0\0\0\0\0\0\0\0" => self.read_inv(body_reader),
                    b"getdata\0\0\0\0\0" => self.read_getdata(body_reader),
                    b"notfound\0\0\0\0" => self.read_notfound(body_reader),
                    b"tx\0\0\0\0\0\0\0\0\0\0" => self.read_tx(body_reader),
                    b"mempool\0\0\0\0\0" => self.read_mempool(body_reader),
                    b"filterload\0\0" => self.read_filterload(body_reader),
                    b"filteradd\0\0\0" => self.read_filteradd(body_reader),
                    b"filterclear\0" => self.read_filterclear(body_reader),
                    _ => return Err(Parse("unknown command")),
                }
                // We need Ok(Some(msg)) to signal that we're done decoding.
                // This is also convenient for tracing the parse result.
                .map(|msg| {
                    trace!("finished message decoding");
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
            timestamp: Utc.timestamp(reader.read_i64::<LittleEndian>()?, 0),
            address_recv: (
                PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?),
                reader.read_socket_addr()?,
            ),
            address_from: (
                PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?),
                reader.read_socket_addr()?,
            ),
            nonce: Nonce(reader.read_u64::<LittleEndian>()?),
            user_agent: reader.read_string()?,
            start_height: BlockHeight(reader.read_u32::<LittleEndian>()?),
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
            message: reader.read_string()?,
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
            reason: reader.read_string()?,
            data: Some(reader.read_32_bytes()?),
        })
    }

    fn read_addr<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        use crate::meta_addr::MetaAddr;

        // addrs are encoded as: timestamp + services + ipv6 + port
        const ENCODED_ADDR_SIZE: usize = 4 + 8 + 16 + 2;
        let max_count = self.builder.max_len / ENCODED_ADDR_SIZE;

        let addrs: Vec<MetaAddr> = reader.read_list(max_count)?;

        Ok(Message::Addr(addrs))
    }

    fn read_getaddr<R: Read>(&self, mut _reader: R) -> Result<Message, Error> {
        Ok(Message::GetAddr)
    }

    fn read_block<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::Block {
            version: Version(reader.read_u32::<LittleEndian>()?),

            block: Block::zcash_deserialize(&mut reader)?,
        })
    }

    fn read_getblocks<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        let version = Version(reader.read_u32::<LittleEndian>()?);

        let max_count = self.builder.max_len / 32;

        let block_locator_hashes: Vec<BlockHeaderHash> = reader.read_list(max_count)?;

        let hash_stop = BlockHeaderHash(reader.read_32_bytes()?);

        Ok(Message::GetBlocks {
            version,
            block_locator_hashes,
            hash_stop,
        })
    }

    /// Deserialize a `headers` message.
    ///
    /// See [Zcash block header] for the enumeration of these fields.
    ///
    /// [Zcash block header](https://zips.z.cash/protocol/protocol.pdf#page=84)
    fn read_headers<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        const ENCODED_HEADER_SIZE: usize = 4 + 32 + 32 + 32 + 4 + 4 + 32 + 3 + 1344;
        let max_count = self.builder.max_len / ENCODED_HEADER_SIZE;

        let headers: Vec<BlockHeader> = reader.read_list(max_count)?;

        Ok(Message::Headers(headers))
    }

    fn read_getheaders<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        let version = Version(reader.read_u32::<LittleEndian>()?);

        let max_count = self.builder.max_len / 32;

        let block_locator_hashes: Vec<BlockHeaderHash> = reader.read_list(max_count)?;

        let hash_stop = BlockHeaderHash(reader.read_32_bytes()?);

        Ok(Message::GetHeaders {
            version,
            block_locator_hashes,
            hash_stop,
        })
    }

    fn read_inv<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        // encoding: 4 byte type tag + 32 byte hash
        const ENCODED_INVHASH_SIZE: usize = 4 + 32;
        let max_count = self.builder.max_len / ENCODED_INVHASH_SIZE;

        let hashes: Vec<InventoryHash> = reader.read_list(max_count)?;
        Ok(Message::Inv(hashes))
    }

    fn read_getdata<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        // encoding: 4 byte type tag + 32 byte hash
        const ENCODED_INVHASH_SIZE: usize = 4 + 32;
        let max_count = self.builder.max_len / ENCODED_INVHASH_SIZE;

        let hashes: Vec<InventoryHash> = reader.read_list(max_count)?;
        Ok(Message::GetData(hashes))
    }

    fn read_notfound<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        // encoding: 4 byte type tag + 32 byte hash
        const ENCODED_INVHASH_SIZE: usize = 4 + 32;
        let max_count = self.builder.max_len / ENCODED_INVHASH_SIZE;

        let hashes: Vec<InventoryHash> = reader.read_list(max_count)?;

        Ok(Message::GetData(hashes))
    }

    fn read_tx<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::Tx {
            version: Version(reader.read_u32::<LittleEndian>()?),

            transaction: Transaction::zcash_deserialize(&mut reader)?,
        })
    }

    fn read_mempool<R: Read>(&self, mut _reader: R) -> Result<Message, Error> {
        Ok(Message::Mempool)
    }

    fn read_filterload<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        Ok(Message::FilterLoad {
            filter: Filter::zcash_deserialize(&mut reader)?,
            hash_functions_count: reader.read_u32::<LittleEndian>()?,
            tweak: Tweak(reader.read_u32::<LittleEndian>()?),
            flags: reader.read_u8()?,
        })
    }

    fn read_filteradd<R: Read>(&self, mut reader: R) -> Result<Message, Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;

        Ok(Message::FilterAdd { data: bytes })
    }

    fn read_filterclear<R: Read>(&self, mut _reader: R) -> Result<Message, Error> {
        Ok(Message::FilterClear)
    }
}

// XXX replace these interior unit tests with exterior integration tests + proptest
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;

    #[test]
    fn version_message_round_trip() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let services = PeerServices::NODE_NETWORK;
        let timestamp = Utc.timestamp(1568000000, 0);

        let rt = Runtime::new().unwrap();

        let v = Message::Version {
            version: crate::constants::CURRENT_VERSION,
            services,
            timestamp,
            address_recv: (
                services,
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6)), 8233),
            ),
            address_from: (
                services,
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 6)), 8233),
            ),
            nonce: Nonce(0x9082_4908_8927_9238),
            user_agent: "Zebra".to_owned(),
            start_height: BlockHeight(540_000),
            relay: true,
        };

        use tokio::codec::{FramedRead, FramedWrite};
        use tokio::prelude::*;
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
    fn decode_state_debug() {
        assert_eq!(format!("{:?}", DecodeState::Head), "DecodeState::Head");

        let decode_state = DecodeState::Body {
            body_len: 43,
            command: [118, 101, 114, 115, 105, 111, 110, 0, 0, 0, 0, 0],
            checksum: Sha256dChecksum([186, 250, 162, 227]),
        };

        assert_eq!(format!("{:?}", decode_state),
                   "DecodeState::Body { body_len: 43, command: \"version\\u{0}\\u{0}\\u{0}\\u{0}\\u{0}\", checksum: Sha256dChecksum(\"bafaa2e3\") }");

        let decode_state = DecodeState::Body {
            body_len: 43,
            command: [118, 240, 144, 128, 105, 111, 110, 0, 0, 0, 0, 0],
            checksum: Sha256dChecksum([186, 250, 162, 227]),
        };

        assert_eq!(format!("{:?}", decode_state),
                   "DecodeState::Body { body_len: 43, command: \"v�ion\\u{0}\\u{0}\\u{0}\\u{0}\\u{0}\", checksum: Sha256dChecksum(\"bafaa2e3\") }");
    }
}
