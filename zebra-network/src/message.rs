//! Definitions of network messages.

use std::io;

use byteorder::{LittleEndian, WriteBytesExt};
use chrono::{DateTime, Utc};

use zebra_chain::types::Sha256dChecksum;

use crate::meta_addr::MetaAddr;
use crate::serialization::{ReadZcashExt, SerializationError, WriteZcashExt, ZcashSerialization};
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

        /// The network address of the node receiving this message.
        ///
        /// Note that the timestamp field of the [`MetaAddr`] is not included in
        /// the serialization of `version` messages.
        address_receiving: MetaAddr,

        /// The network address of the node emitting this message.
        ///
        /// Note that the timestamp field of the [`MetaAddr`] is not included in
        /// the serialization of `version` messages.
        address_from: MetaAddr,

        /// Node random nonce, randomly generated every time a version
        /// packet is sent. This nonce is used to detect connections
        /// to self.
        nonce: Nonce,

        /// The Zcash user agent advertised by the sender.
        user_agent: String,

        /// The last block received by the emitting node.
        start_height: zebra_chain::types::BlockHeight,

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
    Ping(Nonce),

    /// A `pong` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#pong)
    Pong(
        /// The nonce from the `Ping` message this was in response to.
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
        data: [u8; 32],
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
    Tx {/* XXX add fields */},

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
    /// Write the body of the message into the given writer. This allows writing
    /// the message body prior to writing the header, so that the header can
    /// contain a checksum of the message body.
    fn write_body<W: io::Write>(
        &self,
        _writer: W,
        _magic: Magic,
        _version: Version,
    ) -> Result<(), SerializationError> {
        unimplemented!()
    }

    /// Try to deserialize a [`Message`] from the given reader.
    pub fn try_read_body<R: io::Read>(
        _reader: R,
        _magic: Magic,
        _version: Version,
    ) -> Result<Self, SerializationError> {
        unimplemented!()
    }
}

impl ZcashSerialization for Message {
    fn write<W: io::Write>(
        &self,
        mut writer: W,
        magic: Magic,
        version: Version,
    ) -> Result<(), SerializationError> {
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

        // Write the header and then the body.
        writer.write_all(&magic.0)?;
        writer.write_all(command)?;
        writer.write_u32::<LittleEndian>(body.len() as u32)?;
        writer.write_all(&Sha256dChecksum::from(&body[..]).0)?;
        writer.write_all(&body)?;

        Ok(())
    }

    /// Try to read `self` from the given `reader`.
    fn try_read<R: io::Read>(
        _reader: R,
        _magic: Magic,
        _version: Version,
    ) -> Result<Self, SerializationError> {
        unimplemented!()
    }
}
