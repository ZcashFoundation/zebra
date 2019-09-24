//! A Tokio codec mapping byte streams to Bitcoin message streams.

use std::io::{Cursor, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use bytes::BytesMut;
use chrono::{TimeZone, Utc};
use failure::Error;
use tokio::codec::{Decoder, Encoder};

use zebra_chain::{
    serialization::{ReadZcashExt, WriteZcashExt},
    types::{BlockHeight, Sha256dChecksum},
};

use crate::{constants, types::*, Network};

use super::message::Message;

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
    ///
    /// # Example
    /// ```
    /// # use zebra_network::protocol::codec::Codec;
    /// use zebra_network::{constants, Network};
    ///
    /// let codec = Codec::builder()
    ///     .for_network(Network::Mainnet)
    ///     .for_version(constants::CURRENT_VERSION)
    ///     .with_max_body_len(4_000_000)
    ///     .finish();
    /// ```
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

/// The length of a Bitcoin message header.
const HEADER_LEN: usize = 24usize;

// ======== Encoding =========

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
                writer.write_u64::<LittleEndian>(services.0)?;
                writer.write_i64::<LittleEndian>(timestamp.timestamp())?;

                let (recv_services, recv_addr) = address_recv;
                writer.write_u64::<LittleEndian>(recv_services.0)?;
                writer.write_socket_addr(*recv_addr)?;

                let (from_services, from_addr) = address_from;
                writer.write_u64::<LittleEndian>(from_services.0)?;
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
            _ => bail!("unimplemented message type"),
        }
        Ok(())
    }
}

impl Encoder for Codec {
    type Item = Message;
    type Error = Error;

    #[instrument(skip(src))]
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
            Inventory { .. } => b"inv\0\0\0\0\0\0\0\0\0", // XXX Inventory -> Inv ?
            GetData { .. } => b"getdata\0\0\0\0\0",
            NotFound { .. } => b"notfound\0\0\0\0",
            Tx { .. } => b"tx\0\0\0\0\0\0\0\0\0\0",
            Mempool { .. } => b"mempool\0\0\0\0\0",
            FilterLoad { .. } => b"filterload\0\0",
            FilterAdd { .. } => b"filteradd\0\0\0",
            FilterClear { .. } => b"filterclear\0",
            MerkleBlock { .. } => b"merkleblock\0",
        };
        trace!(?command, len = body.len());

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

// ======== Decoding =========

#[derive(Debug)]
enum DecodeState {
    Head,
    Body {
        body_len: usize,
        command: [u8; 12],
        checksum: Sha256dChecksum,
    },
}

impl Decoder for Codec {
    type Item = Message;
    type Error = Error;

    #[instrument(skip(src))]
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
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
                trace!(?self.state, ?magic, ?command, body_len, ?checksum, "read header from src buffer");

                ensure!(
                    magic == self.builder.network.magic(),
                    "supplied magic did not meet expectations"
                );
                ensure!(
                    body_len < self.builder.max_len,
                    "body length exceeded maximum size",
                );

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
                // and reset the decoder state for the next message.
                let body = src.split_to(body_len);
                self.state = DecodeState::Head;

                ensure!(
                    checksum == Sha256dChecksum::from(&body[..]),
                    "supplied message checksum does not match computed checksum"
                );

                let body_reader = Cursor::new(&body);
                let v = self.builder.version;
                match &command {
                    b"version\0\0\0\0\0" => try_read_version(body_reader, v),
                    b"verack\0\0\0\0\0\0" => try_read_verack(body_reader, v),
                    b"ping\0\0\0\0\0\0\0\0" => try_read_ping(body_reader, v),
                    b"pong\0\0\0\0\0\0\0\0" => try_read_pong(body_reader, v),
                    b"reject\0\0\0\0\0\0" => try_read_reject(body_reader, v),
                    b"addr\0\0\0\0\0\0\0\0" => try_read_addr(body_reader, v),
                    b"getaddr\0\0\0\0\0" => try_read_getaddr(body_reader, v),
                    b"block\0\0\0\0\0\0\0" => try_read_block(body_reader, v),
                    b"getblocks\0\0\0" => try_read_getblocks(body_reader, v),
                    b"headers\0\0\0\0\0" => try_read_headers(body_reader, v),
                    b"getheaders\0\0" => try_read_getheaders(body_reader, v),
                    b"inv\0\0\0\0\0\0\0\0\0" => try_read_inv(body_reader, v),
                    b"getdata\0\0\0\0\0" => try_read_getdata(body_reader, v),
                    b"notfound\0\0\0\0" => try_read_notfound(body_reader, v),
                    b"tx\0\0\0\0\0\0\0\0\0\0" => try_read_tx(body_reader, v),
                    b"mempool\0\0\0\0\0" => try_read_mempool(body_reader, v),
                    b"filterload\0\0" => try_read_filterload(body_reader, v),
                    b"filteradd\0\0\0" => try_read_filteradd(body_reader, v),
                    b"filterclear\0" => try_read_filterclear(body_reader, v),
                    b"merkleblock\0" => try_read_merkleblock(body_reader, v),
                    _ => bail!("unknown command"),
                }
                // We need Ok(Some(msg)) to signal that we're done decoding
                .map(|msg| Some(msg))
            }
        }
    }
}

fn try_read_version<R: Read>(mut reader: R, _parsing_version: Version) -> Result<Message, Error> {
    Ok(Message::Version {
        version: Version(reader.read_u32::<LittleEndian>()?),
        services: Services(reader.read_u64::<LittleEndian>()?),
        timestamp: Utc.timestamp(reader.read_i64::<LittleEndian>()?, 0),
        address_recv: (
            Services(reader.read_u64::<LittleEndian>()?),
            reader.read_socket_addr()?,
        ),
        address_from: (
            Services(reader.read_u64::<LittleEndian>()?),
            reader.read_socket_addr()?,
        ),
        nonce: Nonce(reader.read_u64::<LittleEndian>()?),
        user_agent: reader.read_string()?,
        start_height: BlockHeight(reader.read_u32::<LittleEndian>()?),
        relay: match reader.read_u8()? {
            0 => false,
            1 => true,
            _ => bail!("non-bool value supplied in relay field"),
        },
    })
}

fn try_read_verack<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    Ok(Message::Verack)
}

fn try_read_ping<R: Read>(mut reader: R, _version: Version) -> Result<Message, Error> {
    Ok(Message::Ping(Nonce(reader.read_u64::<LittleEndian>()?)))
}

fn try_read_pong<R: Read>(mut reader: R, _version: Version) -> Result<Message, Error> {
    Ok(Message::Pong(Nonce(reader.read_u64::<LittleEndian>()?)))
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_reject<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("reject");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_addr<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("addr");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_getaddr<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("getaddr");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_block<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("block");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_getblocks<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("getblocks");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_headers<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("headers");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_getheaders<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("getheaders");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_inv<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("inv");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_getdata<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("getdata");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_notfound<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("notfound");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_tx<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("tx");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_mempool<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("mempool");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_filterload<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("filterload");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_filteradd<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("filteradd");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_filterclear<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("filterclear");
    bail!("unimplemented message type")
}

#[instrument(level = "trace", skip(_reader, _version))]
fn try_read_merkleblock<R: Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    trace!("merkleblock");
    bail!("unimplemented message type")
}

// XXX replace these interior unit tests with exterior integration tests + proptest
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Runtime;

    #[test]
    fn version_message_round_trip() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let services = Services(0x1);
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
}
