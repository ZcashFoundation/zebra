//! Definitions of network messages.

use std::io::{self, Cursor, Read, Write};
use std::net;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};
use failure::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use zebra_chain::{
    serialization::{ReadZcashExt, WriteZcashExt, ZcashDeserialize, ZcashSerialize},
    transaction::Transaction,
    types::{BlockHeight, Sha256dChecksum},
};

use crate::meta_addr::MetaAddr;
use crate::types::*;

/// A Bitcoin-like network message for the Zcash protocol.
///
/// The Zcash network protocol is mostly inherited from Bitcoin, and a list of
/// Bitcoin network messages can be found [on the Bitcoin
/// wiki][btc_wiki_protocol].
///
/// That page describes the wire format of the messages, while this enum stores
/// an internal representation. The internal representation is unlinked from the
/// wire format, and the translation between the two happens only during
/// serialization and deserialization. For instance, Bitcoin identifies messages
/// by a 12-byte ascii command string; we consider this a serialization detail
/// and use the enum discriminant instead. (As a side benefit, this also means
/// that we have a clearly-defined validation boundary for network messages
/// during serialization).
///
/// [btc_wiki_protocol]: https://en.bitcoin.it/wiki/Protocol_documentation
//
// XXX not all messages are filled in yet. Messages written as { /* XXX add
// fields */ } are explicitly incomplete and we need to define a mapping between
// the serialized message data and the internal representation. Note that this
// is different from messages like GetAddr which have no data (and so have no
// fields).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Message {
    /// A `version` message.
    ///
    /// Note that although this is called `version` in Bitcoin, its role is really
    /// analogous to a `ClientHello` message in TLS, used to begin a handshake, and
    /// is distinct from a simple version number.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#version)
    Version {
        /// The network version number supported by the sender.
        version: Version,

        /// The network services advertised by the sender.
        services: Services,

        /// The time when the version message was sent.
        timestamp: DateTime<Utc>,

        /// The network address of the node receiving this message, and its
        /// advertised network services.
        ///
        /// Q: how does the handshake know the remote peer's services already?
        address_recv: (Services, net::SocketAddr),

        /// The network address of the node sending this message, and its
        /// advertised network services.
        address_from: (Services, net::SocketAddr),

        /// Node random nonce, randomly generated every time a version
        /// packet is sent. This nonce is used to detect connections
        /// to self.
        nonce: Nonce,

        /// The Zcash user agent advertised by the sender.
        user_agent: String,

        /// The last block received by the emitting node.
        start_height: BlockHeight,

        /// Whether the remote peer should announce relayed
        /// transactions or not, see [BIP 0037](https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki)
        relay: bool,
    },

    /// A `verack` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#verack)
    Verack,

    /// A `ping` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#ping)
    Ping(
        /// A nonce unique to this [`Ping`] message.
        Nonce,
    ),

    /// A `pong` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#pong)
    Pong(
        /// The nonce from the [`Ping`] message this was in response to.
        Nonce,
    ),

    /// A `reject` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#reject)
    Reject {
        /// Type of message rejected.
        // Q: can we just reference the Type, rather than instantiate an instance of the enum type?
        message: Box<Message>,

        /// RejectReason code relating to rejected message.
        ccode: RejectReason,

        /// Human-readable version of rejection reason.
        reason: String,

        /// Optional extra data provided for some errors.
        // Currently, all errors which provide this field fill it with
        // the TXID or block header hash of the object being rejected,
        // so the field is 32 bytes.
        //
        // Q: can we tell Rust that this field is optional? Or just
        // default its value to an empty array, I guess.
        data: Option<[u8; 32]>,
    },

    /// An `addr` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#addr)
    Addr(Vec<MetaAddr>),

    /// A `getaddr` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getaddr)
    GetAddr,

    /// A `block` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#block)
    Block {/* XXX add fields */},

    /// A `getblocks` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getblocks)
    GetBlocks {/* XXX add fields */},

    /// A `headers` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#headers)
    Headers {/* XXX add fields */},

    /// A `getheaders` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getheaders)
    GetHeaders {/* XXX add fields */},

    /// An `inv` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#inv)
    // XXX the bitcoin reference above suggests this can be 1.8 MB in bitcoin -- maybe
    // larger in Zcash, since Zcash objects could be bigger (?) -- does this tilt towards
    // having serialization be async?
    Inventory {
        /// Number of inventory entries.
        count: u64,

        /// Inventory vectors.
        inventory: Vec<zebra_chain::types::InventoryVector>,
    },

    /// A `getdata` message.
    ///
    /// `getdata` is used in response to `inv`, to retrieve the content of
    /// a specific object, and is usually sent after receiving an `inv`
    /// packet, after filtering known elements.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getdata)
    GetData {
        /// Number of inventory entries.
        count: u64,

        /// Inventory vectors.
        inventory: Vec<zebra_chain::types::InventoryVector>,
    },

    /// A `notfound` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#notfound)
    // See note above on `Inventory`.
    NotFound {
        /// Number of inventory entries.
        count: u64,

        /// Inventory vectors.
        inventory: Vec<zebra_chain::types::InventoryVector>,
    },

    /// A `tx` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#tx)
    // `flag` is not included (it's optional), and therefore
    // `tx_witnesses` aren't either, as they go if `flag` goes.
    Tx {
        /// Transaction data format version (note, this is signed).
        version: Version,

        /// The `Transaction` type itself.
        // XXX Is this ~aesthetic~?
        transaction: Transaction,
    },

    /// A `mempool` message.
    ///
    /// This was defined in [BIP35], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#mempool)
    /// [BIP35]: https://github.com/bitcoin/bips/blob/master/bip-0035.mediawiki
    Mempool {/* XXX add fields */},

    /// A `filterload` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterLoad {/* XXX add fields */},

    /// A `filteradd` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterAdd {/* XXX add fields */},

    /// A `filterclear` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    FilterClear {/* XXX add fields */},

    /// A `merkleblock` message.
    ///
    /// This was defined in [BIP37], which is included in Zcash.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#filterload.2C_filteradd.2C_filterclear.2C_merkleblock)
    /// [BIP37]: https://github.com/bitcoin/bips/blob/master/bip-0037.mediawiki
    MerkleBlock {/* XXX add fields */},
}

/// Reject Reason CCodes
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#reject)
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum RejectReason {
    Malformed = 0x01,
    Invalid = 0x10,
    Obsolete = 0x11,
    Duplicate = 0x12,
    Nonstandard = 0x40,
    Dust = 0x41,
    InsufficientFee = 0x42,
    Checkpoint = 0x43,
}

// Q: how do we want to implement serialization, exactly? do we want to have
// something generic over stdlib Read and Write traits, or over async versions
// of those traits?
//
// Note: because of the way the message structure is defined (checksum comes
// first) we can't write the message headers before collecting the whole body
// into a buffer
//
// Maybe just write some functions and refactor later?

impl Message {
    /// Send `self` to the given async writer (e.g., a network stream).
    pub async fn send<W: Unpin + AsyncWrite>(
        &self,
        mut writer: W,
        magic: Magic,
        version: Version,
    ) -> Result<(), Error> {
        // Because the header contains a checksum of
        // the body data, it must be written first.
        let mut body = Vec::new();
        self.write_body(&mut body, magic, version)?;

        use Message::*;
        // Note: because all match arms must have
        // the same type, and the array length is
        // part of the type, having at least one
        // of length 12 checks that they are all
        // of length 12, as they must be &[u8; 12].
        let command = match *self {
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
            Inventory { .. } => b"inv\0\0\0\0\0\0\0\0\0",
            GetData { .. } => b"getdata\0\0\0\0\0",
            NotFound { .. } => b"notfound\0\0\0\0",
            Tx { .. } => b"tx\0\0\0\0\0\0\0\0\0\0",
            Mempool { .. } => b"mempool\0\0\0\0\0",
            FilterLoad { .. } => b"filterload\0\0",
            FilterAdd { .. } => b"filteradd\0\0\0",
            FilterClear { .. } => b"filterclear\0",
            MerkleBlock { .. } => b"merkleblock\0",
        };

        // Write the header into a stack buffer first before feeding that stack
        // buffer into the async writer. This allows using the WriteBytesExt
        // extension trait, which is only defined for sync Writers.

        // The header is 4+12+4+4=24 bytes long.
        let mut header = [0u8; 24];
        let mut header_writer = Cursor::new(&mut header[..]);
        header_writer.write_all(&magic.0)?;
        header_writer.write_all(command)?;
        header_writer.write_u32::<LittleEndian>(body.len() as u32)?;
        header_writer.write_all(&Sha256dChecksum::from(&body[..]).0)?;

        writer.write_all(&header).await?;
        writer.write_all(&body).await?;

        Ok(())
    }

    /// Receive a message from the given async reader (e.g., a network stream).
    pub async fn recv<R: Unpin + AsyncRead>(
        mut reader: R,
        magic: Magic,
        version: Version,
    ) -> Result<Self, Error> {
        // Read the header into a stack buffer before trying to parse it. This
        // allows using the ReadBytesExt extension trait, which is only defined
        // for sync Readers. Then we can determine the expected message length,
        // await that many bytes, and finally parse the message.

        // The header is 4+12+4+4=24 bytes long.
        let header = {
            let mut bytes = [0u8; 24];
            reader.read_exact(&mut bytes).await?;
            bytes
        };
        let mut header_reader = Cursor::new(&header[..]);

        // Read header data
        let message_magic = Magic(header_reader.read_4_bytes()?);
        let command = header_reader.read_12_bytes()?;
        let body_len = header_reader.read_u32::<LittleEndian>()? as usize;
        let checksum = Sha256dChecksum(header_reader.read_4_bytes()?);

        ensure!(
            magic == message_magic,
            "supplied magic did not meet expectations",
        );

        // XXX bound the body_len value to avoid large attacker-controlled allocs
        // XXX add a ChecksumReader<R: Read>(R) wrapper and avoid this
        let body = {
            let mut bytes = vec![0; body_len];
            reader.read_exact(&mut bytes).await?;
            bytes
        };

        ensure!(
            checksum == Sha256dChecksum::from(&body[..]),
            "supplied message checksum does not match computed checksum"
        );

        let body_reader = Cursor::new(&body);
        match &command {
            b"version\0\0\0\0\0" => try_read_version(body_reader, version),
            b"verack\0\0\0\0\0\0" => try_read_verack(body_reader, version),
            b"ping\0\0\0\0\0\0\0\0" => try_read_ping(body_reader, version),
            b"pong\0\0\0\0\0\0\0\0" => try_read_pong(body_reader, version),
            b"reject\0\0\0\0\0\0" => try_read_reject(body_reader, version),
            b"addr\0\0\0\0\0\0\0\0" => try_read_addr(body_reader, version),
            b"getaddr\0\0\0\0\0" => try_read_getaddr(body_reader, version),
            b"block\0\0\0\0\0\0\0" => try_read_block(body_reader, version),
            b"getblocks\0\0\0" => try_read_getblocks(body_reader, version),
            b"headers\0\0\0\0\0" => try_read_headers(body_reader, version),
            b"getheaders\0\0" => try_read_getheaders(body_reader, version),
            b"inv\0\0\0\0\0\0\0\0\0" => try_read_inv(body_reader, version),
            b"getdata\0\0\0\0\0" => try_read_getdata(body_reader, version),
            b"notfound\0\0\0\0" => try_read_notfound(body_reader, version),
            b"tx\0\0\0\0\0\0\0\0\0\0" => try_read_tx(body_reader, version),
            b"mempool\0\0\0\0\0" => try_read_mempool(body_reader, version),
            b"filterload\0\0" => try_read_filterload(body_reader, version),
            b"filteradd\0\0\0" => try_read_filteradd(body_reader, version),
            b"filterclear\0" => try_read_filterclear(body_reader, version),
            b"merkleblock\0" => try_read_merkleblock(body_reader, version),
            _ => bail!("unknown command"),
        }
    }
}

impl Message {
    /// Write the body of the message into the given writer. This allows writing
    /// the message body prior to writing the header, so that the header can
    /// contain a checksum of the message body.
    fn write_body<W: io::Write>(&self, mut writer: W, _m: Magic, _v: Version) -> Result<(), Error> {
        use Message::*;
        match *self {
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

fn try_read_version<R: io::Read>(
    mut reader: R,
    _parsing_version: Version,
) -> Result<Message, Error> {
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

fn try_read_verack<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    Ok(Message::Verack)
}

fn try_read_ping<R: io::Read>(mut reader: R, _version: Version) -> Result<Message, Error> {
    Ok(Message::Ping(Nonce(reader.read_u64::<LittleEndian>()?)))
}

fn try_read_pong<R: io::Read>(mut reader: R, _version: Version) -> Result<Message, Error> {
    Ok(Message::Pong(Nonce(reader.read_u64::<LittleEndian>()?)))
}

fn try_read_reject<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_addr<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_getaddr<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_block<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_getblocks<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_headers<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_getheaders<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_inv<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_getdata<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_notfound<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_tx<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_mempool<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_filterload<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_filteradd<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_filterclear<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

fn try_read_merkleblock<R: io::Read>(mut _reader: R, _version: Version) -> Result<Message, Error> {
    bail!("unimplemented message type")
}

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

        let v_bytes = rt.block_on(async {
            let mut bytes = Vec::new();
            v.send(
                &mut bytes,
                crate::constants::magics::MAINNET,
                crate::constants::CURRENT_VERSION,
            )
            .await
            .unwrap();
            bytes
        });

        let v_parsed = rt.block_on(async {
            Message::recv(
                Cursor::new(&v_bytes),
                crate::constants::magics::MAINNET,
                crate::constants::CURRENT_VERSION,
            )
            .await
            .unwrap()
        });

        assert_eq!(v, v_parsed);
    }
}
