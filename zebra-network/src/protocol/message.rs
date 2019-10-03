//! Definitions of network messages.

use std::net;

use chrono::{DateTime, Utc};

use zebra_chain::block::{Block, BlockHeader};
use zebra_chain::{transaction::Transaction, types::BlockHeight};

use crate::meta_addr::MetaAddr;

use super::inv::InventoryHash;
use super::types::*;

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
        services: PeerServices,

        /// The time when the version message was sent.
        timestamp: DateTime<Utc>,

        /// The network address of the node receiving this message, and its
        /// advertised network services.
        ///
        /// Q: how does the handshake know the remote peer's services already?
        address_recv: (PeerServices, net::SocketAddr),

        /// The network address of the node sending this message, and its
        /// advertised network services.
        address_from: (PeerServices, net::SocketAddr),

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
    Block {
        /// The block itself.
        block: Block,
    },

    /// A `getblocks` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getblocks)
    GetBlocks {/* XXX add fields */},

    /// A `headers` message.
    ///
    /// Returns block headers in response to a getheaders packet.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#headers)
    // Note that the block headers in this packet include a
    // transaction count (a var_int, so there can be more than 81
    // bytes per header) as opposed to the block headers that are
    // hashed by miners.
    Headers(Vec<BlockHeader>),

    /// A `getheaders` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getheaders)
    GetHeaders {/* XXX add fields */},

    /// An `inv` message.
    ///
    /// Allows a node to advertise its knowledge of one or more
    /// objects. It can be received unsolicited, or in reply to
    /// `getblocks`.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#inv)
    // XXX the bitcoin reference above suggests this can be 1.8 MB in bitcoin -- maybe
    // larger in Zcash, since Zcash objects could be bigger (?) -- does this tilt towards
    // having serialization be async?
    Inv(Vec<InventoryHash>),

    /// A `getdata` message.
    ///
    /// `getdata` is used in response to `inv`, to retrieve the
    /// content of a specific object, and is usually sent after
    /// receiving an `inv` packet, after filtering known elements.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#getdata)
    GetData(Vec<InventoryHash>),

    /// A `notfound` message.
    ///
    /// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#notfound)
    // See note above on `Inventory`.
    NotFound(Vec<InventoryHash>),

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
